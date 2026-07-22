use serde::Deserialize;
use std::path::Path;

use crate::error::AppError;
use crate::platform::OuyiPlatform;

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
    #[serde(default)]
    pub openai_tool_runtime: Option<ToolRuntimeConfig>,
    #[serde(default)]
    pub platform: Option<PlatformConfig>,
    pub ouyi_v7: Option<Ouyi7Config>,
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
    #[serde(default = "default_tool_runtime_enabled")]
    pub enabled: bool,
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

fn default_tool_runtime_enabled() -> bool {
    true
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
pub struct PlatformConfig {
    #[serde(default = "default_platform")]
    pub default: OuyiPlatform,
    #[serde(default = "default_prompt_on_start")]
    pub prompt_on_start: bool,
}

fn default_platform() -> OuyiPlatform {
    OuyiPlatform::Ouyi8
}

fn default_prompt_on_start() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7Config {
    pub base_url: String,
    #[serde(default = "default_ouyi_v7_chatapi_prefix")]
    pub chatapi_prefix: String,
    #[serde(default)]
    pub jwt_token: String,
    #[serde(default = "default_ouyi_v7_model")]
    pub default_model: String,
    #[serde(default = "default_model_refresh_interval_secs")]
    pub model_refresh_interval_secs: u64,
    #[serde(default = "default_ouyi_v7_max_history_messages")]
    pub max_history_messages: usize,
    #[serde(default = "default_ouyi_v7_stream_chunk_chars")]
    pub simulated_stream_chunk_chars: usize,
}

fn default_ouyi_v7_chatapi_prefix() -> String {
    "/chatapi".to_string()
}

fn default_ouyi_v7_model() -> String {
    "ouyi-chat".to_string()
}

fn default_model_refresh_interval_secs() -> u64 {
    3600
}

fn default_ouyi_v7_max_history_messages() -> usize {
    20
}

fn default_ouyi_v7_stream_chunk_chars() -> usize {
    80
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    pub request_timeout_secs: u64,
    pub no_data_timeout_secs: u64,
    pub max_response_bytes: usize,
    pub max_messages_bytes: usize,
    #[serde(default)]
    pub max_concurrent_requests: usize,
}

impl AppConfig {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::var("OUYI_CONFIG").unwrap_or_else(|_| "config.toml".to_string());
        Self::load_from_path(Path::new(&path))
    }

    pub fn load_from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn config_path() -> String {
        std::env::var("OUYI_CONFIG").unwrap_or_else(|_| "config.toml".to_string())
    }
}

/// Persist a complete in-memory configuration as one validated, atomic file update.
///
/// The existing per-section writers remain available for startup/self-healing paths, but
/// admin updates should use this function so a request cannot leave only half of its fields
/// written when a later validation or persistence step fails.
pub fn write_app_config(path: &Path, config: &AppConfig) -> Result<AppConfig, AppError> {
    crate::config_store::update_document(path, |doc| {
        let server = ensure_table(doc, "server")?;
        server["host"] = toml_edit::value(config.server.host.as_str());
        server["port"] = toml_edit::value(i64::from(config.server.port));

        let ouyi = ensure_table(doc, "ouyi")?;
        ouyi["base_url"] = toml_edit::value(config.ouyi.base_url.as_str());
        ouyi["jwt_token"] = toml_edit::value(config.ouyi.jwt_token.as_str());
        ouyi["app_version"] = toml_edit::value(config.ouyi.app_version.as_str());
        ouyi["model_refresh_interval_secs"] =
            toml_edit::value(config.ouyi.model_refresh_interval_secs as i64);

        let limits = ensure_table(doc, "limits")?;
        limits["request_timeout_secs"] =
            toml_edit::value(config.limits.request_timeout_secs as i64);
        limits["no_data_timeout_secs"] =
            toml_edit::value(config.limits.no_data_timeout_secs as i64);
        limits["max_response_bytes"] = toml_edit::value(config.limits.max_response_bytes as i64);
        limits["max_messages_bytes"] = toml_edit::value(config.limits.max_messages_bytes as i64);
        limits["max_concurrent_requests"] = toml_edit::value(
            i64::try_from(config.limits.max_concurrent_requests).map_err(|_| {
                AppError::Config("max_concurrent_requests 超出可持久化范围".to_string())
            })?,
        );

        write_optional_admin(doc, config.admin.as_ref())?;
        write_optional_platform(doc, config.platform.as_ref())?;
        write_optional_ouyi_v7(doc, config.ouyi_v7.as_ref())?;
        write_optional_tool_runtime(doc, "tool_runtime", config.tool_runtime.as_ref())?;
        write_optional_tool_runtime(
            doc,
            "openai_tool_runtime",
            config.openai_tool_runtime.as_ref(),
        )?;
        Ok(())
    })
}

fn ensure_table<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    name: &str,
) -> Result<&'a mut toml_edit::Table, AppError> {
    if !doc.contains_key(name) {
        doc[name] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc[name]
        .as_table_mut()
        .ok_or_else(|| AppError::Config(format!("配置 [{name}] 必须是普通 table")))
}

fn write_optional_admin(
    doc: &mut toml_edit::DocumentMut,
    config: Option<&AdminConfig>,
) -> Result<(), AppError> {
    let Some(config) = config else {
        doc.remove("admin");
        return Ok(());
    };
    let table = ensure_table(doc, "admin")?;
    set_optional_string(table, "jwt_secret", config.jwt_secret.as_deref());
    set_optional_string(table, "password_hash", config.password_hash.as_deref());
    Ok(())
}

fn write_optional_platform(
    doc: &mut toml_edit::DocumentMut,
    config: Option<&PlatformConfig>,
) -> Result<(), AppError> {
    let Some(config) = config else {
        doc.remove("platform");
        return Ok(());
    };
    let table = ensure_table(doc, "platform")?;
    table["default"] = toml_edit::value(match config.default {
        OuyiPlatform::Ouyi7 => "ouyi7",
        OuyiPlatform::Ouyi8 => "ouyi8",
    });
    table["prompt_on_start"] = toml_edit::value(config.prompt_on_start);
    Ok(())
}

fn write_optional_ouyi_v7(
    doc: &mut toml_edit::DocumentMut,
    config: Option<&Ouyi7Config>,
) -> Result<(), AppError> {
    let Some(config) = config else {
        doc.remove("ouyi_v7");
        return Ok(());
    };
    let table = ensure_table(doc, "ouyi_v7")?;
    table["base_url"] = toml_edit::value(config.base_url.as_str());
    table["chatapi_prefix"] = toml_edit::value(config.chatapi_prefix.as_str());
    table["jwt_token"] = toml_edit::value(config.jwt_token.as_str());
    table["default_model"] = toml_edit::value(config.default_model.as_str());
    table["model_refresh_interval_secs"] =
        toml_edit::value(config.model_refresh_interval_secs as i64);
    table["max_history_messages"] = toml_edit::value(config.max_history_messages as i64);
    table["simulated_stream_chunk_chars"] =
        toml_edit::value(config.simulated_stream_chunk_chars as i64);
    Ok(())
}

fn write_optional_tool_runtime(
    doc: &mut toml_edit::DocumentMut,
    name: &str,
    config: Option<&ToolRuntimeConfig>,
) -> Result<(), AppError> {
    let Some(config) = config else {
        doc.remove(name);
        return Ok(());
    };
    let table = ensure_table(doc, name)?;
    table["enabled"] = toml_edit::value(config.enabled);
    table["mode"] = toml_edit::value(match config.mode {
        ToolRuntimeMode::Native => "native",
        ToolRuntimeMode::Compiler => "compiler",
    });
    table["provider"] = toml_edit::value(match config.provider {
        ToolRuntimeProvider::Anthropic => "anthropic",
        ToolRuntimeProvider::OpenaiCompatible => "openai-compatible",
    });
    set_optional_string(table, "base_url", config.base_url.as_deref());
    set_optional_string(table, "api_key", config.api_key.as_deref());
    set_optional_string(table, "api_key_env", config.api_key_env.as_deref());
    set_optional_string(table, "model", config.model.as_deref());
    table["timeout_secs"] = toml_edit::value(config.timeout_secs as i64);
    Ok(())
}

fn set_optional_string(table: &mut toml_edit::Table, key: &str, value: Option<&str>) {
    match value {
        Some(value) => table[key] = toml_edit::value(value),
        None => {
            table.remove(key);
        }
    }
}

pub fn update_ouyi_jwt_token(path: &Path, jwt_token: &str) -> Result<(), crate::error::AppError> {
    crate::config_store::update_document(path, |doc| {
        if !doc.contains_key("ouyi") {
            return Err(crate::error::AppError::Config(
                "配置缺少 [ouyi] 段".to_string(),
            ));
        }
        doc["ouyi"]["jwt_token"] = toml_edit::value(jwt_token);
        Ok(())
    })
    .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Ouyi JWT Token 已更新");
    Ok(())
}

pub fn update_ouyi_v7_jwt_token(
    path: &Path,
    jwt_token: &str,
) -> Result<(), crate::error::AppError> {
    crate::config_store::update_document(path, |doc| {
        if !doc.contains_key("ouyi_v7") {
            let mut table = toml_edit::Table::new();
            table["base_url"] = toml_edit::value("https://api-8.rcouyi.com");
            table["chatapi_prefix"] = toml_edit::value("/chatapi");
            table["jwt_token"] = toml_edit::value(jwt_token);
            table["default_model"] = toml_edit::value("ouyi-chat");
            table["model_refresh_interval_secs"] = toml_edit::value(1800i64);
            table["max_history_messages"] = toml_edit::value(20i64);
            table["simulated_stream_chunk_chars"] = toml_edit::value(80i64);
            doc["ouyi_v7"] = toml_edit::Item::Table(table);
        } else {
            doc["ouyi_v7"]["jwt_token"] = toml_edit::value(jwt_token);
        }
        Ok(())
    })
    .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Ouyi V7 JWT Token 已更新");
    Ok(())
}

pub fn write_admin_config(
    path: &Path,
    jwt_secret: &str,
    password_hash: &str,
) -> Result<(), crate::error::AppError> {
    crate::config_store::update_document(path, |doc| {
        if !doc.contains_key("admin") {
            doc["admin"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["admin"]["jwt_secret"] = toml_edit::value(jwt_secret);
        doc["admin"]["password_hash"] = toml_edit::value(password_hash);
        Ok(())
    })
    .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Admin 配置已更新");
    Ok(())
}

#[cfg(test)]
pub fn write_limits_config(path: &Path, config: &LimitsConfig) -> Result<(), AppError> {
    let max_concurrent_requests = i64::try_from(config.max_concurrent_requests)
        .map_err(|_| AppError::Config("max_concurrent_requests 超出可持久化范围".to_string()))?;

    let content = std::fs::read_to_string(path)
        .map_err(|e| AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::Config(format!("解析配置失败: {}", e)))?;

    if !doc.contains_key("limits") {
        doc["limits"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let limits = doc
        .get_mut("limits")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| AppError::Config("配置 [limits] 必须是普通 table".to_string()))?;
    limits["max_concurrent_requests"] = toml_edit::value(max_concurrent_requests);

    crate::config_store::write_document(path, &doc)
        .map_err(|e| AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Limits 配置已更新");
    Ok(())
}

#[cfg(test)]
pub fn write_ouyi_config(path: &Path, config: &OuyiConfig) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    if !doc.contains_key("ouyi") {
        doc["ouyi"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["ouyi"]["base_url"] = toml_edit::value(config.base_url.as_str());
    doc["ouyi"]["jwt_token"] = toml_edit::value(config.jwt_token.as_str());
    doc["ouyi"]["app_version"] = toml_edit::value(config.app_version.as_str());
    doc["ouyi"]["model_refresh_interval_secs"] =
        toml_edit::value(config.model_refresh_interval_secs as i64);

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Ouyi 配置已更新");
    Ok(())
}

#[cfg(test)]
pub fn write_ouyi_v7_config(
    path: &Path,
    config: &Ouyi7Config,
) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    if !doc.contains_key("ouyi_v7") {
        doc["ouyi_v7"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["ouyi_v7"]["base_url"] = toml_edit::value(config.base_url.as_str());
    doc["ouyi_v7"]["chatapi_prefix"] = toml_edit::value(config.chatapi_prefix.as_str());
    doc["ouyi_v7"]["jwt_token"] = toml_edit::value(config.jwt_token.as_str());
    doc["ouyi_v7"]["default_model"] = toml_edit::value(config.default_model.as_str());
    doc["ouyi_v7"]["model_refresh_interval_secs"] =
        toml_edit::value(config.model_refresh_interval_secs as i64);
    doc["ouyi_v7"]["max_history_messages"] = toml_edit::value(config.max_history_messages as i64);
    doc["ouyi_v7"]["simulated_stream_chunk_chars"] =
        toml_edit::value(config.simulated_stream_chunk_chars as i64);

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Ouyi V7 配置已更新");
    Ok(())
}

#[cfg(test)]
pub fn write_server_config(
    path: &Path,
    config: &ServerConfig,
) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    if !doc.contains_key("server") {
        doc["server"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["server"]["host"] = toml_edit::value(config.host.as_str());
    doc["server"]["port"] = toml_edit::value(config.port as i64);

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("Server 配置已更新");
    Ok(())
}

#[cfg(test)]
pub fn write_platform_config(
    path: &Path,
    config: &PlatformConfig,
) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    if !doc.contains_key("platform") {
        doc["platform"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let platform = match config.default {
        OuyiPlatform::Ouyi7 => "ouyi7",
        OuyiPlatform::Ouyi8 => "ouyi8",
    };
    doc["platform"]["default"] = toml_edit::value(platform);
    doc["platform"]["prompt_on_start"] = toml_edit::value(config.prompt_on_start);

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    tracing::info!("平台配置已更新");
    Ok(())
}

#[cfg(test)]
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
    table["enabled"] = toml_edit::value(config.enabled);
    table["mode"] = toml_edit::value(match config.mode {
        ToolRuntimeMode::Native => "native",
        ToolRuntimeMode::Compiler => "compiler",
    });
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

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    Ok(())
}

#[cfg(test)]
pub fn remove_tool_runtime_config(path: &Path) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;
    doc.remove("tool_runtime");

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    Ok(())
}

#[cfg(test)]
pub fn write_openai_tool_runtime_config(
    path: &Path,
    config: &ToolRuntimeConfig,
) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;

    let mut table = toml_edit::Table::new();
    table["enabled"] = toml_edit::value(config.enabled);
    table["mode"] = toml_edit::value(match config.mode {
        ToolRuntimeMode::Native => "native",
        ToolRuntimeMode::Compiler => "compiler",
    });
    table["provider"] = toml_edit::value("openai-compatible");
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
    doc["openai_tool_runtime"] = toml_edit::Item::Table(table);

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    Ok(())
}

#[cfg(test)]
pub fn remove_openai_tool_runtime_config(path: &Path) -> Result<(), crate::error::AppError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| crate::error::AppError::Config(format!("读取配置失败: {}", e)))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| crate::error::AppError::Config(format!("解析配置失败: {}", e)))?;
    doc.remove("openai_tool_runtime");

    crate::config_store::write_document(path, &doc)
        .map_err(|e| crate::error::AppError::Config(format!("写入配置失败: {}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_concurrent_requests_defaults_to_unlimited() {
        let limits: LimitsConfig = toml::from_str(
            r#"
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576
"#,
        )
        .unwrap();

        assert_eq!(limits.max_concurrent_requests, 0);
    }

    #[test]
    fn test_load_from_path_reads_persisted_concurrency() {
        let path =
            std::env::temp_dir().join(format!("ouyi-config-load-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            r#"
[server]
host = "127.0.0.1"
port = 3000
[ouyi]
base_url = "https://example.invalid"
jwt_token = "test"
app_version = "3.0.3"
model_refresh_interval_secs = 3600
[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576
max_concurrent_requests = 2
"#,
        )
        .unwrap();

        let config = AppConfig::load_from_path(&path).unwrap();

        assert_eq!(config.limits.max_concurrent_requests, 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_write_app_config_persists_concurrency_and_runtime_flags_together() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-full-config-write-{}.toml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            r#"# keep this comment
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.invalid"
jwt_token = "test"
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576
max_concurrent_requests = 1

[custom]
preserved = true
"#,
        )
        .unwrap();

        let mut candidate = AppConfig::load_from_path(&path).unwrap();
        candidate.limits.max_concurrent_requests = 2;
        candidate.tool_runtime = Some(ToolRuntimeConfig {
            enabled: false,
            mode: ToolRuntimeMode::Native,
            provider: ToolRuntimeProvider::Anthropic,
            base_url: Some("https://tool.example.invalid".to_string()),
            api_key: None,
            api_key_env: Some("TOOL_API_KEY".to_string()),
            model: Some("glm-5.2".to_string()),
            timeout_secs: 45,
        });

        let persisted = write_app_config(&path, &candidate).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let document = content.parse::<toml_edit::DocumentMut>().unwrap();

        assert_eq!(persisted.limits.max_concurrent_requests, 2);
        assert_eq!(
            document["limits"]["max_concurrent_requests"].as_integer(),
            Some(2)
        );
        assert_eq!(document["tool_runtime"]["enabled"].as_bool(), Some(false));
        assert_eq!(document["tool_runtime"]["mode"].as_str(), Some("native"));
        assert_eq!(document["custom"]["preserved"].as_bool(), Some(true));
        assert!(content.contains("# keep this comment"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_write_limits_config_preserves_other_sections() {
        struct TempFileGuard(std::path::PathBuf);

        impl Drop for TempFileGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let temp_file = TempFileGuard(std::env::temp_dir().join(format!(
            "ouyi-limits-config-test-{}.toml",
            uuid::Uuid::new_v4()
        )));
        std::fs::write(
            &temp_file.0,
            r#"# 保留顶层注释
[server]
host = "127.0.0.1"
port = 3000

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 20
max_response_bytes = 1048576
max_messages_bytes = 524288
custom_limit = "keep-me"
"#,
        )
        .unwrap();

        write_limits_config(
            &temp_file.0,
            &LimitsConfig {
                request_timeout_secs: 180,
                no_data_timeout_secs: 60,
                max_response_bytes: 2097152,
                max_messages_bytes: 2097152,
                max_concurrent_requests: 42,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(&temp_file.0).unwrap();
        let doc = content.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(content.contains("# 保留顶层注释"));
        assert_eq!(doc["server"]["host"].as_str(), Some("127.0.0.1"));
        assert_eq!(doc["server"]["port"].as_integer(), Some(3000));
        assert_eq!(doc["limits"]["request_timeout_secs"].as_integer(), Some(30));
        assert_eq!(doc["limits"]["custom_limit"].as_str(), Some("keep-me"));
        assert_eq!(
            doc["limits"]["max_concurrent_requests"].as_integer(),
            Some(42)
        );
    }

    #[test]
    fn test_write_limits_config_rejects_oversized_concurrency_without_rewriting_file() {
        struct TempFileGuard(std::path::PathBuf);

        impl Drop for TempFileGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let temp_file = TempFileGuard(std::env::temp_dir().join(format!(
            "ouyi-limits-oversized-test-{}.toml",
            uuid::Uuid::new_v4()
        )));
        let original = r#"[limits]
request_timeout_secs = 30
no_data_timeout_secs = 20
max_response_bytes = 1048576
max_messages_bytes = 524288
max_concurrent_requests = 7
"#;
        std::fs::write(&temp_file.0, original).unwrap();

        let result = write_limits_config(
            &temp_file.0,
            &LimitsConfig {
                request_timeout_secs: 30,
                no_data_timeout_secs: 20,
                max_response_bytes: 1048576,
                max_messages_bytes: 524288,
                max_concurrent_requests: usize::MAX,
            },
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&temp_file.0).unwrap(), original);
    }

    #[test]
    fn test_write_limits_config_rejects_non_table_limits_without_rewriting_file() {
        struct TempFileGuard(std::path::PathBuf);

        impl Drop for TempFileGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let temp_file = TempFileGuard(std::env::temp_dir().join(format!(
            "ouyi-limits-non-table-test-{}.toml",
            uuid::Uuid::new_v4()
        )));
        let original = r#"limits = []

[server]
host = "127.0.0.1"
port = 3000
"#;
        std::fs::write(&temp_file.0, original).unwrap();

        let result = write_limits_config(
            &temp_file.0,
            &LimitsConfig {
                request_timeout_secs: 30,
                no_data_timeout_secs: 20,
                max_response_bytes: 1048576,
                max_messages_bytes: 524288,
                max_concurrent_requests: 8,
            },
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&temp_file.0).unwrap(), original);
    }

    #[test]
    fn test_openai_tool_runtime_config() {
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

[openai_tool_runtime]
enabled = true
provider = "openai-compatible"
base_url = "https://tool.example.com/v1"
api_key = "sk-test"
model = "tool-model"
timeout_secs = 90
"#,
        )
        .unwrap();

        let runtime = config.openai_tool_runtime.unwrap();
        assert!(runtime.enabled);
        assert_eq!(runtime.provider, ToolRuntimeProvider::OpenaiCompatible);
        assert_eq!(
            runtime.base_url.as_deref(),
            Some("https://tool.example.com/v1")
        );
        assert_eq!(runtime.api_key.as_deref(), Some("sk-test"));
        assert_eq!(runtime.model.as_deref(), Some("tool-model"));
        assert_eq!(runtime.timeout_secs, 90);
    }

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
            enabled: false,
            mode: ToolRuntimeMode::Native,
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

    #[test]
    fn test_write_admin_config_creates_section() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-admin-config-test-{}.toml",
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

        write_admin_config(&path, "secret-value", "bcrypt-hash").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let config: AppConfig = toml::from_str(&content).unwrap();
        let admin = config.admin.unwrap();
        assert_eq!(admin.jwt_secret.as_deref(), Some("secret-value"));
        assert_eq!(admin.password_hash.as_deref(), Some("bcrypt-hash"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_write_ouyi_config_updates_existing_section() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-config-write-test-{}.toml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://old.example.com"
jwt_token = "old-token"
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

        write_ouyi_config(
            &path,
            &OuyiConfig {
                base_url: "https://new.example.com".to_string(),
                jwt_token: "new-token".to_string(),
                app_version: "3.1.0".to_string(),
                model_refresh_interval_secs: 1800,
            },
        )
        .unwrap();

        let config: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.ouyi.base_url, "https://new.example.com");
        assert_eq!(config.ouyi.jwt_token, "new-token");
        assert_eq!(config.ouyi.app_version, "3.1.0");
        assert_eq!(config.ouyi.model_refresh_interval_secs, 1800);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_write_ouyi_v7_config_creates_section() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-v7-config-write-test-{}.toml",
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

        write_ouyi_v7_config(
            &path,
            &Ouyi7Config {
                base_url: "https://api-8.rcouyi.com".to_string(),
                chatapi_prefix: "/chatapi".to_string(),
                jwt_token: "v7-token".to_string(),
                default_model: "ouyi-chat".to_string(),
                model_refresh_interval_secs: 900,
                max_history_messages: 12,
                simulated_stream_chunk_chars: 64,
            },
        )
        .unwrap();

        let config: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let v7 = config.ouyi_v7.unwrap();
        assert_eq!(v7.jwt_token, "v7-token");
        assert_eq!(v7.model_refresh_interval_secs, 900);
        assert_eq!(v7.max_history_messages, 12);
        assert_eq!(v7.simulated_stream_chunk_chars, 64);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_ouyi_v7_config_all_fields() {
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

[ouyi_v7]
base_url = "https://api-8.rcouyi.com"
chatapi_prefix = "/chatapi"
jwt_token = "test-v7-token"
default_model = "ouyi-chat"
model_refresh_interval_secs = 1800
max_history_messages = 30
simulated_stream_chunk_chars = 100
"#,
        )
        .unwrap();

        let v7 = config.ouyi_v7.unwrap();
        assert_eq!(v7.base_url, "https://api-8.rcouyi.com");
        assert_eq!(v7.chatapi_prefix, "/chatapi");
        assert_eq!(v7.jwt_token, "test-v7-token");
        assert_eq!(v7.default_model, "ouyi-chat");
        assert_eq!(v7.model_refresh_interval_secs, 1800);
        assert_eq!(v7.max_history_messages, 30);
        assert_eq!(v7.simulated_stream_chunk_chars, 100);
    }

    #[test]
    fn test_ouyi_v7_config_defaults() {
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

[ouyi_v7]
base_url = "https://api-8.rcouyi.com"
"#,
        )
        .unwrap();

        let v7 = config.ouyi_v7.unwrap();
        assert_eq!(v7.chatapi_prefix, "/chatapi");
        assert_eq!(v7.jwt_token, "");
        assert_eq!(v7.default_model, "ouyi-chat");
        assert_eq!(v7.model_refresh_interval_secs, 3600);
        assert_eq!(v7.max_history_messages, 20);
        assert_eq!(v7.simulated_stream_chunk_chars, 80);
    }

    #[test]
    fn test_platform_config_default_ouyi7() {
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

[platform]
default = "ouyi7"
prompt_on_start = true
"#,
        )
        .unwrap();

        let platform = config.platform.unwrap();
        assert_eq!(platform.default, crate::platform::OuyiPlatform::Ouyi7);
        assert!(platform.prompt_on_start);
    }

    #[test]
    fn test_platform_config_default_ouyi8() {
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

[platform]
default = "ouyi8"
"#,
        )
        .unwrap();

        let platform = config.platform.unwrap();
        assert_eq!(platform.default, crate::platform::OuyiPlatform::Ouyi8);
        assert!(platform.prompt_on_start);
    }

    #[test]
    fn test_platform_config_missing_defaults_to_ouyi8() {
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

        assert!(config.platform.is_none());
    }

    #[test]
    fn test_update_ouyi_v7_jwt_token_only_updates_v7() {
        let path =
            std::env::temp_dir().join(format!("ouyi-v7-jwt-test-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://example.com"
jwt_token = "original-v8-token"
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[ouyi_v7]
base_url = "https://api-8.rcouyi.com"
jwt_token = "original-v7-token"
"#,
        )
        .unwrap();

        update_ouyi_v7_jwt_token(&path, "new-v7-token").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let config: AppConfig = toml::from_str(&content).unwrap();

        assert_eq!(config.ouyi.jwt_token, "original-v8-token");
        assert_eq!(config.ouyi_v7.unwrap().jwt_token, "new-v7-token");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_update_ouyi_jwt_token_preserves_v7() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-jwt-preserve-v7-test-{}.toml",
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
jwt_token = "original-v8-token"
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[ouyi_v7]
base_url = "https://api-8.rcouyi.com"
jwt_token = "original-v7-token"
"#,
        )
        .unwrap();

        update_ouyi_jwt_token(&path, "new-v8-token").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let config: AppConfig = toml::from_str(&content).unwrap();

        assert_eq!(config.ouyi.jwt_token, "new-v8-token");
        assert_eq!(config.ouyi_v7.unwrap().jwt_token, "original-v7-token");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_update_ouyi_v7_jwt_token_creates_section_when_missing() {
        let path = std::env::temp_dir().join(format!(
            "ouyi-v7-create-section-test-{}.toml",
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
jwt_token = "original-v8-token"
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

        update_ouyi_v7_jwt_token(&path, "newly-created-v7-token").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let config: AppConfig = toml::from_str(&content).unwrap();

        let v7 = config.ouyi_v7.unwrap();
        assert_eq!(v7.base_url, "https://api-8.rcouyi.com");
        assert_eq!(v7.jwt_token, "newly-created-v7-token");
        assert_eq!(config.ouyi.jwt_token, "original-v8-token");

        let _ = std::fs::remove_file(path);
    }
}
