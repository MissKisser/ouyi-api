//! 平台选择与状态模块。
//!
//! 定义 Ouyi 平台枚举（7.0 / 8.0）及其显示名、环境变量取值、配置段名的映射，
//! 提供启动时的平台选择决策链（环境变量 `OUYI_PLATFORM` 优先 → 配置缺省平台 + 交互询问
//! → 配置缺省平台），以及供管理后台展示的平台运行状态结构。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Ouyi 平台枚举，标识反代服务对接的平台代际。
/// JSON 序列化形式为小写字符串 `"ouyi7"` / `"ouyi8"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OuyiPlatform {
    /// Ouyi 7.0 平台，对应配置段 `ouyi_v7`。
    Ouyi7,
    /// Ouyi 8.0 平台，对应配置段 `ouyi`。
    Ouyi8,
}

impl OuyiPlatform {
    /// 返回平台显示名：`"Ouyi 7.0"` 或 `"Ouyi 8.0"`。
    pub fn label(&self) -> &'static str {
        match self {
            OuyiPlatform::Ouyi7 => "Ouyi 7.0",
            OuyiPlatform::Ouyi8 => "Ouyi 8.0",
        }
    }

    /// 解析环境变量取值为平台，大小写不敏感。
    /// 接受 `"7"` / `"v7"` / `"ouyi7"` 与 `"8"` / `"v8"` / `"ouyi8"`，其余返回 `None`。
    pub fn from_env_value(s: &str) -> Option<Self> {
        let s_lower = s.to_lowercase();
        match s_lower.as_str() {
            "7" | "v7" | "ouyi7" => Some(OuyiPlatform::Ouyi7),
            "8" | "v8" | "ouyi8" => Some(OuyiPlatform::Ouyi8),
            _ => None,
        }
    }

    /// 返回该平台在配置文件中对应的配置段名：7.0 → `ouyi_v7`，8.0 → `ouyi`。
    pub fn config_key(&self) -> &'static str {
        match self {
            OuyiPlatform::Ouyi7 => "ouyi_v7",
            OuyiPlatform::Ouyi8 => "ouyi",
        }
    }
}

/// 平台运行状态，供管理后台状态接口展示。
#[derive(Debug, Clone, Serialize)]
pub struct PlatformStatus {
    /// 当前平台。
    pub platform: OuyiPlatform,
    /// 平台显示名。
    pub label: String,
    /// 平台 API 基础地址。
    pub base_url: String,
    /// 当前已加载的模型数量。
    pub model_count: usize,
    /// 平台 Token 是否已验证有效。
    pub token_valid: bool,
    /// 最近一次创建/刷新该状态的时间（UTC）。
    pub last_selected_at: DateTime<Utc>,
}

impl PlatformStatus {
    /// 创建初始平台状态：模型数为 0、Token 未验证、时间戳取当前 UTC 时刻。
    pub fn new(platform: OuyiPlatform, base_url: String) -> Self {
        Self {
            label: platform.label().to_string(),
            platform,
            base_url,
            model_count: 0,
            token_valid: false,
            last_selected_at: Utc::now(),
        }
    }
}

/// 解析交互式平台选择输入，自动去除首尾空白、大小写不敏感。
/// `"1"` / `"7"` / `"v7"` / `"ouyi7"` → 7.0，`"2"` / `"8"` / `"v8"` / `"ouyi8"` → 8.0，
/// 其余输入返回 `None`。
pub fn parse_platform_choice(input: &str) -> Option<OuyiPlatform> {
    match input.trim().to_lowercase().as_str() {
        "1" | "7" | "v7" | "ouyi7" => Some(OuyiPlatform::Ouyi7),
        "2" | "8" | "v8" | "ouyi8" => Some(OuyiPlatform::Ouyi8),
        _ => None,
    }
}

/// 交互式选择平台：向终端打印选项并循环读取输入，直到得到合法选择。
/// - `is_terminal`: 进程是否连接交互终端；为 `false` 时直接返回 `Err`，不发起询问。
/// 副作用：写标准输出、读标准输入（阻塞等待用户输入）。
/// 返回 `Err(String)`：非交互终端、输入流关闭或读取失败。
pub fn select_platform(is_terminal: bool) -> Result<OuyiPlatform, String> {
    if !is_terminal {
        return Err(
            "当前启动方式没有可用控制台，无法选择平台。请使用 bat 或终端启动 ouyi-api.exe。"
                .to_string(),
        );
    }

    use std::io::{self, BufRead, Write};

    println!("请选择要连接的平台:");
    println!("  1. Ouyi 7.0");
    println!("  2. Ouyi 8.0");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        print!("请输入序号 (1/2) 后回车: ");
        let _ = io::stdout().flush();

        let Some(line) = lines.next() else {
            return Err("未读取到平台选择".to_string());
        };

        let line = line.map_err(|e| format!("读取平台选择失败: {}", e))?;
        if let Some(platform) = parse_platform_choice(&line) {
            return Ok(platform);
        }

        println!("无效输入，请输入 1 或 2。");
    }
}

/// 依据配置与环境变量决定启动平台。
/// 决策链：环境变量 `OUYI_PLATFORM`（存在即生效，非法值报错）→ 交互终端且配置允许询问时
/// 交互选择 → 配置缺省平台（未配置 `[platform]` 段时为 8.0）。
/// 副作用：走交互分支时会写标准输出、读标准输入。
/// 返回 `Err(String)`：环境变量取值非法，或需要交互选择但没有终端。
pub fn select_platform_from_config(
    config: Option<&crate::config::PlatformConfig>,
    is_terminal: bool,
) -> Result<OuyiPlatform, String> {
    let env_platform = std::env::var("OUYI_PLATFORM").ok();
    select_platform_from_config_and_env(config, env_platform.as_deref(), is_terminal)
}

/// [`select_platform_from_config`] 的纯函数版本：环境变量取值由调用方注入，便于测试。
/// 决策链与错误情形与其一致。
pub fn select_platform_from_config_and_env(
    config: Option<&crate::config::PlatformConfig>,
    env_platform: Option<&str>,
    is_terminal: bool,
) -> Result<OuyiPlatform, String> {
    if let Some(value) = env_platform {
        return OuyiPlatform::from_env_value(value)
            .ok_or_else(|| format!("无效 Ouyi 平台环境变量 OUYI_PLATFORM: {}", value));
    }

    let default = config
        .map(|config| config.default)
        .unwrap_or(OuyiPlatform::Ouyi8);
    let prompt_on_start = config.map(|config| config.prompt_on_start).unwrap_or(true);

    if is_terminal && prompt_on_start {
        select_platform(true)
    } else {
        Ok(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_label() {
        assert_eq!(OuyiPlatform::Ouyi7.label(), "Ouyi 7.0");
        assert_eq!(OuyiPlatform::Ouyi8.label(), "Ouyi 8.0");
    }

    #[test]
    fn test_platform_from_env_value() {
        assert_eq!(OuyiPlatform::from_env_value("7"), Some(OuyiPlatform::Ouyi7));
        assert_eq!(
            OuyiPlatform::from_env_value("v7"),
            Some(OuyiPlatform::Ouyi7)
        );
        assert_eq!(
            OuyiPlatform::from_env_value("ouyi7"),
            Some(OuyiPlatform::Ouyi7)
        );
        assert_eq!(
            OuyiPlatform::from_env_value("OUYI7"),
            Some(OuyiPlatform::Ouyi7)
        );
        assert_eq!(OuyiPlatform::from_env_value("8"), Some(OuyiPlatform::Ouyi8));
        assert_eq!(
            OuyiPlatform::from_env_value("v8"),
            Some(OuyiPlatform::Ouyi8)
        );
        assert_eq!(
            OuyiPlatform::from_env_value("ouyi8"),
            Some(OuyiPlatform::Ouyi8)
        );
        assert_eq!(OuyiPlatform::from_env_value("invalid"), None);
    }

    #[test]
    fn test_platform_config_key() {
        assert_eq!(OuyiPlatform::Ouyi7.config_key(), "ouyi_v7");
        assert_eq!(OuyiPlatform::Ouyi8.config_key(), "ouyi");
    }

    #[test]
    fn test_parse_platform_choice() {
        assert_eq!(parse_platform_choice("1"), Some(OuyiPlatform::Ouyi7));
        assert_eq!(parse_platform_choice("7"), Some(OuyiPlatform::Ouyi7));
        assert_eq!(parse_platform_choice("ouyi7"), Some(OuyiPlatform::Ouyi7));
        assert_eq!(parse_platform_choice("2"), Some(OuyiPlatform::Ouyi8));
        assert_eq!(parse_platform_choice("8"), Some(OuyiPlatform::Ouyi8));
        assert_eq!(parse_platform_choice("ouyi8"), Some(OuyiPlatform::Ouyi8));
        assert_eq!(parse_platform_choice(""), None);
        assert_eq!(parse_platform_choice("invalid"), None);
    }

    #[test]
    fn test_select_platform_requires_terminal() {
        let result = select_platform(false);
        assert!(result.is_err());
    }

    #[test]
    fn test_select_platform_from_config_uses_default_without_terminal() {
        let config = crate::config::PlatformConfig {
            default: OuyiPlatform::Ouyi7,
            prompt_on_start: true,
        };

        let result = select_platform_from_config(Some(&config), false);

        assert_eq!(result.unwrap(), OuyiPlatform::Ouyi7);
    }

    #[test]
    fn test_select_platform_env_overrides_config_default() {
        let config = crate::config::PlatformConfig {
            default: OuyiPlatform::Ouyi8,
            prompt_on_start: false,
        };

        let result = select_platform_from_config_and_env(Some(&config), Some("ouyi7"), false);

        assert_eq!(result.unwrap(), OuyiPlatform::Ouyi7);
    }

    #[test]
    fn test_select_platform_env_rejects_invalid_value() {
        let result = select_platform_from_config_and_env(None, Some("bad"), false);

        assert!(result.is_err());
    }

    #[test]
    fn test_select_platform_from_config_skips_prompt_when_disabled() {
        let config = crate::config::PlatformConfig {
            default: OuyiPlatform::Ouyi7,
            prompt_on_start: false,
        };

        let result = select_platform_from_config(Some(&config), true);

        assert_eq!(result.unwrap(), OuyiPlatform::Ouyi7);
    }

    #[test]
    fn test_select_platform_from_config_defaults_to_ouyi8() {
        let result = select_platform_from_config(None, false);

        assert_eq!(result.unwrap(), OuyiPlatform::Ouyi8);
    }

    #[test]
    fn test_platform_status_new() {
        let status =
            PlatformStatus::new(OuyiPlatform::Ouyi7, "https://api.example.com".to_string());
        assert_eq!(status.platform, OuyiPlatform::Ouyi7);
        assert_eq!(status.label, "Ouyi 7.0");
        assert_eq!(status.base_url, "https://api.example.com");
        assert_eq!(status.model_count, 0);
        assert!(!status.token_valid);
    }

    #[test]
    fn test_platform_serialization() {
        let json = serde_json::to_string(&OuyiPlatform::Ouyi7).unwrap();
        assert_eq!(json, "\"ouyi7\"");

        let json = serde_json::to_string(&OuyiPlatform::Ouyi8).unwrap();
        assert_eq!(json, "\"ouyi8\"");
    }

    #[test]
    fn test_platform_deserialization() {
        let platform: OuyiPlatform = serde_json::from_str("\"ouyi7\"").unwrap();
        assert_eq!(platform, OuyiPlatform::Ouyi7);

        let platform: OuyiPlatform = serde_json::from_str("\"ouyi8\"").unwrap();
        assert_eq!(platform, OuyiPlatform::Ouyi8);
    }
}
