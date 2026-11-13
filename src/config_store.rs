//! 配置文件持久化模块。
//!
//! 提供对 `config.toml` 的"读取 → 修改 → 校验 → 原子替换"写入通道：
//! - 进程内互斥锁（`CONFIG_FILE_LOCK`）串行化所有写入，避免并发写互相覆盖；
//! - 修改后的候选文本先经 `AppConfig` 反序列化校验，失败则放弃写入，原文件保持不变；
//! - 替换通过"同目录唯一临时文件 + rename"实现：Unix 下继承原文件权限并 fsync 目录，
//!   Windows 下目标存在时先移除再 rename；任一步失败都会清理临时文件。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

#[cfg(unix)]
use std::fs::File;

use crate::config::AppConfig;
use crate::error::AppError;

static CONFIG_FILE_LOCK: Mutex<()> = Mutex::new(());

/// 以回调方式修改配置文件并原子持久化。
///
/// 执行顺序（全程持有进程内配置写锁）：
/// 1. 读取 `path` 的原始文本并解析为 `toml_edit::DocumentMut`，保留注释与未知段落；
/// 2. 调用 `mutate` 原地修改文档；`mutate` 返回错误时立即中止，原文件保持不变；
/// 3. 将候选文本反序列化为 [`AppConfig`] 校验合法性，失败则中止且原文件不变；
/// 4. 通过同目录临时文件 + rename 原子替换目标文件（Unix 继承原文件权限并 fsync 目录）。
///
/// - `path`: 配置文件路径。
/// - `mutate`: 对 TOML 文档的原地修改闭包，返回 `Err` 时放弃整次写入。
/// - 返回：替换成功后重新解析出的 `AppConfig`。
/// - 返回 `Err(AppError::Config)`：文件读取、解析、候选校验或替换失败；
///   `Err(AppError::Internal)`：进程内配置写锁已损坏。
pub fn update_document<F>(path: &Path, mutate: F) -> Result<AppConfig, AppError>
where
    F: FnOnce(&mut toml_edit::DocumentMut) -> Result<(), AppError>,
{
    let _guard = CONFIG_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Internal("配置文件锁已损坏".to_string()))?;
    let content = fs::read_to_string(path)
        .map_err(|error| AppError::Config(format!("读取配置失败: {error}")))?;
    let mut document = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| AppError::Config(format!("解析配置失败: {error}")))?;
    mutate(&mut document)?;
    let candidate = document.to_string();
    let config: AppConfig = toml::from_str(&candidate)
        .map_err(|error| AppError::Config(format!("候选配置校验失败: {error}")))?;
    atomic_replace_unlocked(path, candidate.as_bytes())?;
    Ok(config)
}

/// （仅测试）将整份 TOML 文档原子写入 `path`，不做候选校验。
/// 与 [`update_document`] 共用同一把进程内写锁与原子替换流程。
/// 返回 `Err(AppError::Internal)`：写锁损坏；`Err(AppError::Config)`：临时文件或替换失败。
#[cfg(test)]
pub fn write_document(path: &Path, document: &toml_edit::DocumentMut) -> Result<(), AppError> {
    let _guard = CONFIG_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Internal("配置文件锁已损坏".to_string()))?;
    atomic_replace_unlocked(path, document.to_string().as_bytes())
}

/// （仅测试）将原始字节原子写入 `path`，绕过文档解析与候选校验。
/// 错误情形与 [`write_document`] 相同。
#[cfg(test)]
pub fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let _guard = CONFIG_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Internal("配置文件锁已损坏".to_string()))?;
    atomic_replace_unlocked(path, bytes)
}

/// 解析目标文件的父目录；裸文件名（无目录部分）返回当前目录 `.`。
fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// 原子替换文件内容（调用方需已持有配置写锁）。
/// 在同目录创建唯一命名的临时文件写入 `bytes`（Unix 下继承原文件权限），`sync_all` 落盘后
/// rename 到目标路径（Windows 下目标存在时先移除）；Unix 下再 fsync 父目录保证落盘顺序。
/// 任一步失败都会尽力删除临时文件并返回 `Err(AppError::Config)`。
fn atomic_replace_unlocked(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = parent_directory(path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options
            .open(&temp_path)
            .map_err(|error| AppError::Config(format!("创建配置临时文件失败: {error}")))?;
        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(path) {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))
                .map_err(|error| AppError::Config(format!("设置配置权限失败: {error}")))?;
        }
        file.write_all(bytes)
            .map_err(|error| AppError::Config(format!("写入配置临时文件失败: {error}")))?;
        file.sync_all()
            .map_err(|error| AppError::Config(format!("同步配置临时文件失败: {error}")))?;
        drop(file);

        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)
                .map_err(|error| AppError::Config(format!("替换旧配置失败: {error}")))?;
        }
        fs::rename(&temp_path, path)
            .map_err(|error| AppError::Config(format!("原子替换配置失败: {error}")))?;

        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| AppError::Config(format!("同步配置目录失败: {error}")))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相对裸文件名（如 `config.toml`）的 `parent()` 返回空路径而非 `None`；
    /// 空路径 `open()` 在 Linux 上报 ENOENT，会让已成功的写入被目录 fsync
    /// 整体判为失败。此测试锁定裸文件名必须解析到当前目录。
    #[test]
    fn bare_relative_config_path_resolves_parent_to_cwd() {
        assert_eq!(parent_directory(Path::new("config.toml")), Path::new("."));
        assert_eq!(
            parent_directory(Path::new("/etc/ouyi/config.toml")),
            Path::new("/etc/ouyi")
        );
    }


    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ouyi-config-store-{label}-{}.toml",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn atomic_replace_commits_valid_document() {
        let path = temp_path("commit");
        atomic_replace(&path, b"[limits]\nmax_concurrent_requests = 2\n").unwrap();
        let parsed: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            parsed["limits"]["max_concurrent_requests"].as_integer(),
            Some(2)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutator_error_keeps_original_bytes() {
        let path = temp_path("error");
        std::fs::write(&path, "[limits]\nmax_concurrent_requests = 1\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let result = update_document(&path, |_doc| Err(AppError::Config("reject".to_string())));
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn candidate_deserialization_error_keeps_original_bytes() {
        let path = temp_path("invalid");
        std::fs::write(&path, "[limits]\nmax_concurrent_requests = 1\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let result = update_document(&path, |doc| {
            doc["limits"]["max_concurrent_requests"] = toml_edit::value("not-an-integer");
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_file(path);
    }
}
