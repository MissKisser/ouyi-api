use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub ouyi: OuyiConfig,
    pub limits: LimitsConfig,
    pub admin: Option<AdminConfig>,
    #[serde(default)]
    pub tool_call: ToolCallConfig,
    #[serde(default)]
    pub tool_runtime: Option<ToolRuntimeConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolCallConfig {
    #[serde(default)]
    pub extra_starts: Vec<String>,
    #[serde(default)]
    pub extra_ends: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ToolRuntimeConfig {
    #[serde(default = "default_tool_runtime_mode")]
    pub mode: ToolRuntimeMode,
    pub provider: ToolRuntimeProvider,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_tool_runtime_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolRuntimeMode {
    Native,
    Compiler,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRuntimeProvider {
    Anthropic,
    OpenaiCompatible,
}

fn default_tool_runtime_timeout_secs() -> u64 {
    120
}

fn default_tool_runtime_mode() -> ToolRuntimeMode {
    ToolRuntimeMode::Compiler
}

impl ToolRuntimeConfig {
    pub fn get_api_key(&self) -> Result<String, crate::error::AppError> {
        if let Some(key) = &self.api_key {
            return Ok(key.clone());
        }
        if let Some(env_name) = &self.api_key_env {
            if let Ok(key) = std::env::var(env_name) {
                return Ok(key);
            }
            return Err(crate::error::AppError::Config(format!(
                "环境变量 {} 未设置",
                env_name
            )));
        }
        Err(crate::error::AppError::Config(
            "tool_runtime 未配置 api_key 或 api_key_env".to_string(),
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminConfig {
    pub jwt_secret: Option<String>,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OuyiConfig {
    pub base_url: String,
    pub jwt_token: String,
    pub app_version: String,
    pub model_refresh_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    pub request_timeout_secs: u64,
    pub no_data_timeout_secs: u64,
    pub max_response_bytes: usize,
    pub max_messages_bytes: usize,
}

impl AppConfig {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::var("OUYI_CONFIG").unwrap_or_else(|_| "config.toml".to_string());
        let content = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn config_path() -> String {
        std::env::var("OUYI_CONFIG").unwrap_or_else(|_| "config.toml".to_string())
    }
}

pub fn update_ouyi_jwt_token(path: &Path, jwt_token: &str) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    if !doc.contains_key("ouyi") {
        return Err(crate::error::AppError::Config(
            "配置缺少 [ouyi] 段".to_string(),
        ));
    }

    doc["ouyi"]["jwt_token"] = toml_edit::value(jwt_token);

    std::fs::write(path, doc.to_string())
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Ouyi JWT Token 已更新");
    Ok(())
}

pub fn write_tool_runtime_config(
    path: &Path,
    config: &ToolRuntimeConfig,
) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    let mut table = toml_edit::Table::new();
    table["provider"] = toml_edit::value(match config.provider {
        ToolRuntimeProvider::Anthropic => "anthropic",
        ToolRuntimeProvider::OpenaiCompatible => "openai-compatible",
    });
    if let Some(base_url) = &config.base_url {
        table["base_url"] = toml_edit::value(base_url.as_str());
    }
    if let Some(api_key) = &config.api_key {
        table["api_key"] = toml_edit::value(api_key.as_str());
    }
    if let Some(api_key_env) = &config.api_key_env {
        table["api_key_env"] = toml_edit::value(api_key_env.as_str());
    }
    if let Some(model) = &config.model {
        table["model"] = toml_edit::value(model.as_str());
    }
    table["timeout_secs"] = toml_edit::value(config.timeout_secs as i64);
    doc["tool_runtime"] = toml_edit::Item::Table(table);

    std::fs::write(path, doc.to_string())
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    Ok(())
}

pub fn remove_tool_runtime_config(path: &Path) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;
    doc.remove("tool_runtime");

    std::fs::write(path, doc.to_string())
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_runtime_defaults_to_disabled() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576
"#,
        )
        .unwrap();

        assert!(config.tool_runtime.is_none());
    }

    #[test]
    fn test_tool_runtime_anthropic_config_defaults_to_compiler() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[tool_runtime]
provider = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "sk-test-key"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-7"
timeout_secs = 120
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert_eq!(runtime.mode, ToolRuntimeMode::Compiler);
        assert_eq!(runtime.provider, ToolRuntimeProvider::Anthropic);
        assert_eq!(
            runtime.base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(runtime.api_key.as_deref(), Some("sk-test-key"));
        assert_eq!(runtime.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(runtime.model.as_deref(), Some("claude-sonnet-4-7"));
        assert_eq!(runtime.timeout_secs, 120);
    }

    #[test]
    fn test_tool_runtime_compiler_openai_compatible_config() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[tool_runtime]
provider = "openai-compatible"
base_url = "https://api.example.com/v1"
api_key = "sk-compiler-key"
model = "cheap-tool-model"
timeout_secs = 30
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert_eq!(runtime.mode, ToolRuntimeMode::Compiler);
        assert_eq!(runtime.provider, ToolRuntimeProvider::OpenaiCompatible);
        assert_eq!(
            runtime.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(runtime.api_key.as_deref(), Some("sk-compiler-key"));
        assert!(runtime.api_key_env.is_none());
        assert_eq!(runtime.model.as_deref(), Some("cheap-tool-model"));
        assert_eq!(runtime.timeout_secs, 30);
    }

    #[test]
    fn test_get_api_key_from_config() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[tool_runtime]
provider = "openai-compatible"
base_url = "https://api.example.com/v1"
api_key = "sk-direct-key"
model = "cheap-tool-model"
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert_eq!(runtime.get_api_key().unwrap(), "sk-direct-key");
    }

    #[test]
    fn test_get_api_key_from_env() {
        unsafe { std::env::set_var("TEST_COMPILER_API_KEY", "sk-env-key") };
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[tool_runtime]
provider = "openai-compatible"
base_url = "https://api.example.com/v1"
api_key_env = "TEST_COMPILER_API_KEY"
model = "cheap-tool-model"
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert_eq!(runtime.get_api_key().unwrap(), "sk-env-key");
        unsafe { std::env::remove_var("TEST_COMPILER_API_KEY") };
    }

    #[test]
    fn test_get_api_key_missing() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[tool_runtime]
provider = "openai-compatible"
base_url = "https://api.example.com/v1"
model = "cheap-tool-model"
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert!(runtime.get_api_key().is_err());
    }

    #[test]
    fn test_tool_runtime_legacy_native_mode_is_read_for_compatibility() {
        let config: AppConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[tool_runtime]
mode = "native"
provider = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "sk-test-key"
model = "claude-sonnet-4-7"
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert_eq!(runtime.mode, ToolRuntimeMode::Native);
        assert_eq!(runtime.provider, ToolRuntimeProvider::Anthropic);
    }

    #[test]
    fn test_write_and_remove_tool_runtime_config() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-tool-runtime-test-{}.toml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = ""
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576
"#,
        )
        .unwrap();

        let runtime = ToolRuntimeConfig {
            mode: ToolRuntimeMode::Compiler,
            provider: ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some("https://api.example.com/v1".to_string()),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            model: Some("cheap-tool-model".to_string()),
            timeout_secs: 30,
        };

        write_tool_runtime_config(&path, &runtime).unwrap();
        let config: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.tool_runtime.unwrap(), runtime);

        remove_tool_runtime_config(&path).unwrap();
        let config: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(config.tool_runtime.is_none());

        let _ = std::fs::remove_file(path);
    }
}
