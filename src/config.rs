use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub ouyi: OuyiConfig,
    pub limits: LimitsConfig,
    pub admin: Option<AdminConfig>,
    #[serde(default)]
    pub tool_call: ToolCallConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolCallConfig {
    #[serde(default)]
    pub extra_starts: Vec<String>,
    #[serde(default)]
    pub extra_ends: Vec<String>,
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
