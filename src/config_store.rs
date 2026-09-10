use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

#[cfg(unix)]
use std::fs::File;

use crate::config::AppConfig;
use crate::error::AppError;

static CONFIG_FILE_LOCK: Mutex<()> = Mutex::new(());

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

#[cfg(test)]
pub fn write_document(path: &Path, document: &toml_edit::DocumentMut) -> Result<(), AppError> {
    let _guard = CONFIG_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Internal("配置文件锁已损坏".to_string()))?;
    atomic_replace_unlocked(path, document.to_string().as_bytes())
}

#[cfg(test)]
pub fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let _guard = CONFIG_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Internal("配置文件锁已损坏".to_string()))?;
    atomic_replace_unlocked(path, bytes)
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

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
