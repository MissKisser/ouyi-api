use serde::Deserialize;

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
    pub mode: ToolRuntimeMode,
    pub provider: ToolRuntimeProvider,
    #[serde(default)]
    pub base_url: Option<String>,
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
    fn test_tool_runtime_native_anthropic_config() {
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
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-7"
timeout_secs = 120
"#,
        )
        .unwrap();

        let runtime = config.tool_runtime.unwrap();
        assert_eq!(runtime.mode, ToolRuntimeMode::Native);
        assert_eq!(runtime.provider, ToolRuntimeProvider::Anthropic);
        assert_eq!(
            runtime.base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(runtime.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(runtime.model.as_deref(), Some("claude-sonnet-4-7"));
        assert_eq!(runtime.timeout_secs, 120);
    }
}
