use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use reqwest::StatusCode as ReqwestStatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::AppState;
use crate::admin::accounts::AccountEntry;
use crate::client::http::OuyiHttpClient;
use crate::config::{
    AdminConfig, AppConfig, Ouyi7Config, PlatformConfig, ToolRuntimeConfig, ToolRuntimeMode,
    ToolRuntimeProvider, write_admin_config, write_app_config,
};
#[cfg(test)]
use crate::config::{
    remove_openai_tool_runtime_config, remove_tool_runtime_config, write_limits_config,
    write_openai_tool_runtime_config, write_ouyi_config, write_ouyi_v7_config,
    write_platform_config, write_server_config, write_tool_runtime_config,
};
use crate::error::AppError;
use crate::handler::models::list_models as list_openai_models;
use crate::model::mapper::ModelCategory;
use bcrypt::{DEFAULT_COST, hash};

// ── Setup / Login ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    token: String,
}

pub async fn admin_setup(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    let mut store = state.admin_store.write().await;
    if store.setup_completed {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Setup already completed"
            })),
        ));
    }
    let password_hash = match crate::admin::auth::AdminStore::hash_password(&req.password) {
        Ok(hash) => hash,
        Err(_) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Password hashing failed"
                })),
            ));
        }
    };
    let config_path = std::path::PathBuf::from(AppConfig::config_path());
    if let Err(e) = write_admin_config(&config_path, &store.jwt_secret, &password_hash) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        ));
    }
    store.set_password_hash(password_hash);
    let token = match store.generate_token("admin") {
        Ok(t) => t,
        Err(_) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Token generation failed"
                })),
            ));
        }
    };
    Ok(Json(LoginResponse { token }))
}

pub async fn admin_login(
    State(state): State<Arc<AppState>>,
    extract: axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    let store = state.admin_store.read().await;

    if !store.setup_completed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Setup required"
            })),
        ));
    }

    let client_ip = extract.0.ip().to_string();
    if !store.check_rate_limit(&client_ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "Too many login attempts, please try again later"
            })),
        ));
    }

    if !store.verify_password(&req.password) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Invalid password"
            })),
        ));
    }

    let token = match store.generate_token("admin") {
        Ok(t) => t,
        Err(_) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Token generation failed"
                })),
            ));
        }
    };
    Ok(Json(LoginResponse { token }))
}

// ── Status ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AdminStatusResponse {
    accounts: Vec<serde_json::Value>,
    total: usize,
    idle: usize,
    busy: usize,
    error: usize,
    invalid: usize,
    platform: PlatformStatusView,
}

#[derive(Serialize)]
pub struct PlatformStatusView {
    platform: String,
    label: String,
    base_url: String,
    model_count: usize,
    token_valid: bool,
    last_selected_at: Option<String>,
}

pub async fn admin_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let stats = state.account_store.pool_stats().await;
    let accounts = state.account_store.get_accounts().await;
    let runtime = state.account_store.get_runtime_stats().await;

    let account_values: Vec<serde_json::Value> = accounts
        .iter()
        .map(|a| {
            let rt = runtime.iter().find(|(email, _)| email == &a.email);
            let state_str = rt
                .map(|(_, s)| match s.state {
                    crate::admin::accounts::AccountState::Idle => "idle",
                    crate::admin::accounts::AccountState::Busy => "busy",
                    crate::admin::accounts::AccountState::Error { .. } => "error",
                    crate::admin::accounts::AccountState::Invalid => "invalid",
                })
                .unwrap_or("unknown");
            serde_json::json!({
                "email": a.email,
                "enabled": a.enabled,
                "has_token": a.token.is_some(),
                "state": state_str,
            })
        })
        .collect();

    let platform_status = state.platform_status.read().await;
    let platform_view = PlatformStatusView {
        platform: serde_json::to_string(&platform_status.platform)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string(),
        label: platform_status.label.clone(),
        base_url: platform_status.base_url.clone(),
        model_count: platform_status.model_count,
        token_valid: platform_status.token_valid,
        last_selected_at: Some(platform_status.last_selected_at.to_rfc3339()),
    };

    Json(AdminStatusResponse {
        accounts: account_values,
        total: stats.total,
        idle: stats.idle,
        busy: stats.busy,
        error: stats.error,
        invalid: stats.invalid,
        platform: platform_view,
    })
}

// ── Stats ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatsSnapshot {
    total_requests: u64,
    success_requests: u64,
    failed_requests: u64,
    avg_latency_ms: u64,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    uptime_secs: u64,
    models: HashMap<String, ModelStatsEntry>,
    keys: HashMap<String, KeyStatsEntry>,
}

#[derive(Serialize)]
pub struct ModelStatsEntry {
    prompt_tokens: u64,
    completion_tokens: u64,
    requests: u64,
}

#[derive(Serialize)]
pub struct KeyStatsEntry {
    prompt_tokens: u64,
    completion_tokens: u64,
    requests: u64,
}

pub async fn admin_stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let collector = &state.stats;
    let stats = &collector.stats;

    use std::sync::atomic::Ordering;
    let total = stats.total_requests.load(Ordering::Relaxed);
    let success = stats.successful_requests.load(Ordering::Relaxed);
    let failed = stats.failed_requests.load(Ordering::Relaxed);
    let total_latency = stats.total_latency_ms.load(Ordering::Relaxed);
    let total_prompt = stats.total_prompt_tokens.load(Ordering::Relaxed);
    let total_completion = stats.total_completion_tokens.load(Ordering::Relaxed);

    let avg_latency = if total > 0 { total_latency / total } else { 0 };

    let mut models_map: HashMap<String, ModelStatsEntry> = HashMap::new();
    let logs = collector.get_request_logs(1000);
    for log in &logs {
        let entry = models_map
            .entry(log.model.clone())
            .or_insert(ModelStatsEntry {
                prompt_tokens: 0,
                completion_tokens: 0,
                requests: 0,
            });
        entry.requests += 1;
        entry.prompt_tokens += log.prompt_tokens;
        entry.completion_tokens += log.completion_tokens;
    }

    // 合并工具运行时模型统计（tool-runtime::<model> 格式）
    let tool_runtime_stats = collector.get_model_stats();
    for (model_key, requests, prompt_tokens, completion_tokens) in tool_runtime_stats {
        let entry = models_map.entry(model_key).or_insert(ModelStatsEntry {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 0,
        });
        entry.requests += requests;
        entry.prompt_tokens += prompt_tokens;
        entry.completion_tokens += completion_tokens;
    }

    Json(StatsSnapshot {
        total_requests: total,
        success_requests: success,
        failed_requests: failed,
        avg_latency_ms: avg_latency,
        total_prompt_tokens: total_prompt,
        total_completion_tokens: total_completion,
        uptime_secs: collector.uptime_secs(),
        models: models_map,
        keys: HashMap::new(),
    })
}

// ── Models ────────────────────────────────────────────────────────────────

pub async fn admin_models(state: State<Arc<AppState>>) -> impl IntoResponse {
    list_openai_models(state).await.into_response()
}

// ── Config ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AdminConfigResponse {
    password_set: bool,
    jwt_issued_at: u64,
}

#[derive(Serialize)]
pub struct ToolCallTagConfig {
    extra_starts: Vec<String>,
    extra_ends: Vec<String>,
}

#[derive(Serialize)]
pub struct DeepSeekConfigView {
    api_base: String,
    wasm_url: String,
    user_agent: String,
    client_version: String,
    client_platform: String,
    client_locale: String,
    model_types: Vec<String>,
    max_input_tokens: Vec<u32>,
    max_output_tokens: Vec<u32>,
    model_aliases: Vec<String>,
    tool_call: ToolCallTagConfig,
    /// JWT token for ouyi API auth (对应后端 ouyi.jwt_token)
    jwt_token: String,
    model_refresh_interval_secs: u64,
}

#[derive(Serialize)]
pub struct ServerConfigView {
    host: String,
    port: u16,
    cors_origins: Vec<String>,
}

#[derive(Serialize)]
pub struct ProxyConfigView {
    url: Option<String>,
}

#[derive(Serialize)]
pub struct ToolRuntimeConfigView {
    enabled: bool,
    provider: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    timeout_secs: u64,
}

#[derive(Serialize)]
pub struct PlatformConfigView {
    default: String,
    prompt_on_start: bool,
}

#[derive(Serialize)]
pub struct Ouyi7ConfigView {
    base_url: String,
    chatapi_prefix: String,
    jwt_token: String,
    default_model: String,
    model_refresh_interval_secs: u64,
    max_history_messages: usize,
    simulated_stream_chunk_chars: usize,
}

#[derive(Serialize)]
pub struct LimitsConfigView {
    max_concurrent_requests: usize,
}

#[derive(Serialize)]
pub struct FullConfigResponse {
    server: ServerConfigView,
    limits: LimitsConfigView,
    deepseek: DeepSeekConfigView,
    proxy: ProxyConfigView,
    admin: AdminConfigResponse,
    accounts: Vec<serde_json::Value>,
    api_keys: Vec<serde_json::Value>,
    tool_runtime: ToolRuntimeConfigView,
    openai_tool_runtime: ToolRuntimeConfigView,
    platform: Option<PlatformConfigView>,
    ouyi_v7: Option<Ouyi7ConfigView>,
}

pub async fn admin_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store = state.admin_store.read().await;
    let config = state.config.read().await.clone();
    let accounts = state.account_store.get_accounts().await;

    let account_values: Vec<serde_json::Value> = accounts
        .iter()
        .map(|a| {
            serde_json::json!({
                "email": a.email,
                "mobile": a.mobile,
                "area_code": a.area_code,
                "password": "",
                "token": a.token.as_ref().map(|t| mask_account_token(t)).unwrap_or_default(),
                "enabled": a.enabled,
            })
        })
        .collect();

    let tool_runtime_view = if let Some(tr) = &config.tool_runtime {
        ToolRuntimeConfigView {
            enabled: true,
            provider: Some(match tr.provider {
                crate::config::ToolRuntimeProvider::Anthropic => "anthropic".to_string(),
                crate::config::ToolRuntimeProvider::OpenaiCompatible => {
                    "openai-compatible".to_string()
                }
            }),
            base_url: tr.base_url.clone(),
            api_key: tr.api_key.as_ref().map(|k| mask_api_key(k)),
            model: tr.model.clone(),
            timeout_secs: tr.timeout_secs,
        }
    } else {
        ToolRuntimeConfigView {
            enabled: false,
            provider: None,
            base_url: None,
            api_key: None,
            model: None,
            timeout_secs: 120,
        }
    };

    let openai_tool_runtime_view = if let Some(tr) = &config.openai_tool_runtime {
        ToolRuntimeConfigView {
            enabled: tr.enabled,
            provider: Some("openai-compatible".to_string()),
            base_url: tr.base_url.clone(),
            api_key: tr.api_key.as_ref().map(|k| mask_api_key(k)),
            model: tr.model.clone(),
            timeout_secs: tr.timeout_secs,
        }
    } else {
        ToolRuntimeConfigView {
            enabled: false,
            provider: None,
            base_url: None,
            api_key: None,
            model: None,
            timeout_secs: 120,
        }
    };

    Json(FullConfigResponse {
        server: ServerConfigView {
            host: config.server.host.clone(),
            port: config.server.port,
            cors_origins: vec!["*".to_string()],
        },
        limits: LimitsConfigView {
            max_concurrent_requests: config.limits.max_concurrent_requests,
        },
        deepseek: DeepSeekConfigView {
            api_base: config.ouyi.base_url.clone(),
            wasm_url: String::new(),
            user_agent: String::new(),
            client_version: config.ouyi.app_version.clone(),
            client_platform: String::new(),
            client_locale: String::new(),
            model_types: vec![],
            max_input_tokens: vec![],
            max_output_tokens: vec![],
            model_aliases: vec![],
            tool_call: ToolCallTagConfig {
                extra_starts: vec![],
                extra_ends: vec![],
            },
            jwt_token: {
                let t = &config.ouyi.jwt_token;
                if t.len() > 20 {
                    format!("{}...{}", &t[..8], &t[t.len() - 12..])
                } else {
                    t.clone()
                }
            },
            model_refresh_interval_secs: config.ouyi.model_refresh_interval_secs,
        },
        proxy: ProxyConfigView { url: None },
        admin: AdminConfigResponse {
            password_set: store.setup_completed,
            jwt_issued_at: 0,
        },
        accounts: account_values,
        api_keys: vec![],
        tool_runtime: tool_runtime_view,
        openai_tool_runtime: openai_tool_runtime_view,
        platform: config.platform.as_ref().map(|p| PlatformConfigView {
            default: serde_json::to_string(&p.default)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            prompt_on_start: p.prompt_on_start,
        }),
        ouyi_v7: config.ouyi_v7.as_ref().map(|v7| {
            let jwt_masked = if v7.jwt_token.len() > 20 {
                format!(
                    "{}...{}",
                    &v7.jwt_token[..8],
                    &v7.jwt_token[v7.jwt_token.len() - 12..]
                )
            } else {
                v7.jwt_token.clone()
            };
            Ouyi7ConfigView {
                base_url: v7.base_url.clone(),
                chatapi_prefix: v7.chatapi_prefix.clone(),
                jwt_token: jwt_masked,
                default_model: v7.default_model.clone(),
                model_refresh_interval_secs: v7.model_refresh_interval_secs,
                max_history_messages: v7.max_history_messages,
                simulated_stream_chunk_chars: v7.simulated_stream_chunk_chars,
            }
        }),
    })
}

fn mask_api_key(key: &str) -> String {
    if key.len() > 12 {
        format!("{}...{}", &key[..8], &key[key.len() - 4..])
    } else if key.len() > 4 {
        format!("{}...", &key[..4])
    } else {
        "***".to_string()
    }
}

fn is_masked_secret(value: &str) -> bool {
    value.contains("...") || value.contains("***")
}

fn mask_account_token(token: &str) -> String {
    if token.len() > 16 {
        format!("{}...{}", &token[..8], &token[token.len() - 8..])
    } else if token.is_empty() {
        String::new()
    } else {
        "***".to_string()
    }
}

fn parse_tool_runtime_provider(
    value: Option<String>,
    fallback: ToolRuntimeProvider,
) -> ToolRuntimeProvider {
    match value.as_deref() {
        Some("anthropic") => ToolRuntimeProvider::Anthropic,
        Some("openai-compatible") => ToolRuntimeProvider::OpenaiCompatible,
        _ => fallback,
    }
}

fn build_tool_runtime_config(
    current: Option<&ToolRuntimeConfig>,
    update: ToolRuntimeConfigUpdate,
) -> Option<ToolRuntimeConfig> {
    if update.enabled == Some(false) {
        return None;
    }

    let mut next = current.cloned().unwrap_or(ToolRuntimeConfig {
        enabled: true,
        mode: ToolRuntimeMode::Compiler,
        provider: ToolRuntimeProvider::OpenaiCompatible,
        base_url: None,
        api_key: None,
        api_key_env: None,
        model: None,
        timeout_secs: 30,
    });

    next.mode = ToolRuntimeMode::Compiler;
    next.provider = parse_tool_runtime_provider(update.provider, next.provider);
    if let Some(base_url) = update.base_url {
        next.base_url = if base_url.trim().is_empty() {
            None
        } else {
            Some(base_url)
        };
    }
    if let Some(api_key) = update.api_key
        && !api_key.trim().is_empty()
        && !is_masked_secret(&api_key)
    {
        next.api_key = Some(api_key);
    }
    if let Some(model) = update.model {
        next.model = if model.trim().is_empty() {
            None
        } else {
            Some(model)
        };
    }
    if let Some(timeout_secs) = update.timeout_secs {
        next.timeout_secs = timeout_secs;
    }

    Some(next)
}

fn build_openai_tool_runtime_config(
    current: Option<&ToolRuntimeConfig>,
    update: ToolRuntimeConfigUpdate,
) -> Option<ToolRuntimeConfig> {
    if update.enabled == Some(false) {
        return None;
    }

    let mut next = current.cloned().unwrap_or(ToolRuntimeConfig {
        enabled: true,
        mode: ToolRuntimeMode::Compiler,
        provider: ToolRuntimeProvider::OpenaiCompatible,
        base_url: None,
        api_key: None,
        api_key_env: None,
        model: None,
        timeout_secs: 30,
    });

    next.mode = ToolRuntimeMode::Compiler;
    next.provider = ToolRuntimeProvider::OpenaiCompatible;
    if let Some(base_url) = update.base_url {
        next.base_url = if base_url.trim().is_empty() {
            None
        } else {
            Some(base_url)
        };
    }
    if let Some(api_key) = update.api_key
        && !api_key.trim().is_empty()
        && !is_masked_secret(&api_key)
    {
        next.api_key = Some(api_key);
    }
    if let Some(model) = update.model {
        next.model = if model.trim().is_empty() {
            None
        } else {
            Some(model)
        };
    }
    if let Some(timeout_secs) = update.timeout_secs {
        next.timeout_secs = timeout_secs;
    }

    Some(next)
}

fn build_tool_runtime_test_config(
    current: Option<&ToolRuntimeConfig>,
    update: ToolRuntimeTestRequest,
) -> Option<ToolRuntimeConfig> {
    build_tool_runtime_config(
        current,
        ToolRuntimeConfigUpdate {
            enabled: update.enabled,
            provider: update.provider,
            base_url: update.base_url,
            api_key: update.api_key,
            model: update.model,
            timeout_secs: update.timeout_secs,
        },
    )
}

fn tool_runtime_provider_label(provider: &ToolRuntimeProvider) -> &'static str {
    match provider {
        ToolRuntimeProvider::Anthropic => "anthropic",
        ToolRuntimeProvider::OpenaiCompatible => "openai-compatible",
    }
}

fn default_tool_runtime_base_url(provider: &ToolRuntimeProvider) -> &'static str {
    match provider {
        ToolRuntimeProvider::Anthropic => "https://api.anthropic.com",
        ToolRuntimeProvider::OpenaiCompatible => "",
    }
}

fn resolved_tool_runtime_base_url(config: &ToolRuntimeConfig) -> Result<String, String> {
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or_else(|| default_tool_runtime_base_url(&config.provider))
        .trim()
        .trim_end_matches('/');
    if base_url.is_empty() {
        return Err("tool_runtime.base_url 未配置".to_string());
    }
    Ok(base_url.to_string())
}

fn tool_runtime_test_url(config: &ToolRuntimeConfig) -> Result<String, String> {
    let base_url = resolved_tool_runtime_base_url(config)?;
    match &config.provider {
        ToolRuntimeProvider::OpenaiCompatible => Ok(format!("{}/chat/completions", base_url)),
        ToolRuntimeProvider::Anthropic => {
            let base_url = base_url.strip_suffix("/v1").unwrap_or(&base_url);
            Ok(format!("{}/v1/messages", base_url))
        }
    }
}

fn validate_tool_runtime_test_config(config: &ToolRuntimeConfig) -> Result<(), String> {
    if config.model.as_deref().unwrap_or("").trim().is_empty() {
        return Err("tool_runtime.model 未配置".to_string());
    }
    if config.api_key.as_deref().unwrap_or("").trim().is_empty()
        && config
            .api_key_env
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        return Err("tool_runtime.api_key 未配置".to_string());
    }
    let _ = tool_runtime_test_url(config)?;
    Ok(())
}

fn tool_runtime_test_error(status: ReqwestStatusCode, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("工具模型请求失败: HTTP {}", status)
    } else {
        let brief: String = body.chars().take(300).collect();
        format!("工具模型请求失败: HTTP {} {}", status, brief)
    }
}

async fn send_openai_tool_runtime_test(
    state: &AppState,
    config: &ToolRuntimeConfig,
    url: &str,
    api_key: &str,
) -> Result<(), String> {
    let model = config.model.as_deref().unwrap_or_default();
    let request = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": "ping"
            }
        ],
        "max_tokens": 8,
        "stream": false
    });

    let response = state
        .http_client
        .post(url)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .json(&request)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send()
        .await
        .map_err(|e| format!("工具模型请求失败: {}", e))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(tool_runtime_test_error(status, &body));
    }

    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("工具模型响应不是有效 JSON: {}", e))?;
    if value.get("choices").and_then(|v| v.as_array()).is_none() {
        return Err("工具模型响应缺少 choices 字段".to_string());
    }

    Ok(())
}

async fn send_anthropic_tool_runtime_test(
    state: &AppState,
    config: &ToolRuntimeConfig,
    url: &str,
    api_key: &str,
) -> Result<(), String> {
    let model = config.model.as_deref().unwrap_or_default();
    let request = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": "ping"
            }
        ],
        "max_tokens": 8,
        "stream": false
    });

    let response = state
        .http_client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&request)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send()
        .await
        .map_err(|e| format!("工具模型请求失败: {}", e))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(tool_runtime_test_error(status, &body));
    }

    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("工具模型响应不是有效 JSON: {}", e))?;
    if value.get("content").and_then(|v| v.as_array()).is_none() {
        return Err("工具模型响应缺少 content 字段".to_string());
    }

    Ok(())
}

#[derive(Clone, Deserialize)]
pub struct ConfigUpdate {
    pub server: Option<ServerConfigUpdate>,
    pub limits: Option<LimitsConfigUpdate>,
    pub deepseek: Option<OuyiConfigUpdate>,
    pub proxy: Option<ProxyConfigUpdate>,
    pub admin: Option<AdminUpdate>,
    pub accounts: Option<Vec<AccountEntryRaw>>,
    pub tool_runtime: Option<ToolRuntimeConfigUpdate>,
    pub openai_tool_runtime: Option<ToolRuntimeConfigUpdate>,
    pub platform: Option<PlatformConfigUpdate>,
    pub ouyi_v7: Option<Ouyi7ConfigUpdate>,
}

#[derive(Clone, Deserialize)]
pub struct LimitsConfigUpdate {
    pub max_concurrent_requests: Option<usize>,
}

#[derive(Clone, Deserialize)]
pub struct ToolRuntimeConfigUpdate {
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
pub struct ToolRuntimeTestRequest {
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Serialize)]
pub struct ToolRuntimeTestResponse {
    pub success: bool,
    pub message: String,
    pub latency_ms: u64,
    pub mode: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
}

#[derive(Clone, Deserialize)]
pub struct ServerConfigUpdate {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub cors_origins: Option<Vec<String>>,
}

#[derive(Clone, Deserialize)]
pub struct OuyiConfigUpdate {
    pub api_base: Option<String>,
    pub client_version: Option<String>,
    /// 前端传来的 jwt_token 字段（对应后端 ouyi.jwt_token）
    pub jwt_token: Option<String>,
    pub model_refresh_interval_secs: Option<u64>,
}

#[derive(Clone, Deserialize)]
pub struct PlatformConfigUpdate {
    pub default: Option<String>,
    pub prompt_on_start: Option<bool>,
}

#[derive(Clone, Deserialize)]
pub struct Ouyi7ConfigUpdate {
    pub base_url: Option<String>,
    pub chatapi_prefix: Option<String>,
    pub jwt_token: Option<String>,
    pub default_model: Option<String>,
    pub model_refresh_interval_secs: Option<u64>,
    pub max_history_messages: Option<usize>,
    pub simulated_stream_chunk_chars: Option<usize>,
}

#[derive(Clone, Deserialize)]
pub struct ProxyConfigUpdate {
    pub url: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct AdminUpdate {
    pub old_password: Option<String>,
    pub new_password: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct AccountEntryRaw {
    pub email: String,
    pub mobile: String,
    #[serde(default)]
    pub area_code: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

fn build_account_entry_from_raw(
    raw: AccountEntryRaw,
    existing: Option<&AccountEntry>,
) -> AccountEntry {
    let password = if raw.password.is_empty() {
        existing
            .map(|entry| entry.password.clone())
            .unwrap_or_default()
    } else {
        hash(&raw.password, DEFAULT_COST).unwrap_or_default()
    };

    let token = match raw.token {
        Some(token) if token.trim().is_empty() => None,
        Some(token) if is_masked_secret(&token) => existing.and_then(|entry| entry.token.clone()),
        Some(token) => Some(token),
        None => existing.and_then(|entry| entry.token.clone()),
    };

    AccountEntry {
        email: raw.email,
        mobile: raw.mobile,
        area_code: raw.area_code,
        password,
        token,
        enabled: raw.enabled,
    }
}

const MAX_CONCURRENT_REQUESTS: usize = 1000;
pub(super) static CONFIG_UPDATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug)]
#[cfg(test)]
pub(super) enum LimitsConfigUpdateError {
    Invalid(String),
    Persistence(AppError),
}

#[cfg(test)]
pub(super) async fn apply_limits_config_update(
    state: &AppState,
    config_path: &Path,
    max_concurrent_requests: usize,
) -> Result<(), LimitsConfigUpdateError> {
    if max_concurrent_requests > MAX_CONCURRENT_REQUESTS {
        return Err(LimitsConfigUpdateError::Invalid(format!(
            "max_concurrent_requests must be between 0 and {MAX_CONCURRENT_REQUESTS}"
        )));
    }

    let limits = {
        let config = state.config.read().await;
        let mut limits = config.limits.clone();
        limits.max_concurrent_requests = max_concurrent_requests;
        limits
    };
    let config_path = config_path.to_path_buf();
    tokio::task::spawn_blocking(move || write_limits_config(&config_path, &limits))
        .await
        .map_err(|error| {
            LimitsConfigUpdateError::Persistence(AppError::Internal(format!(
                "Limits config write task failed: {error}"
            )))
        })?
        .map_err(LimitsConfigUpdateError::Persistence)?;
    state.config.write().await.limits.max_concurrent_requests = max_concurrent_requests;
    state.concurrency_limiter.set_limit(max_concurrent_requests);
    Ok(())
}

pub(super) struct PreparedConfigUpdate {
    server_changed: bool,
    accounts: Option<Vec<AccountEntryRaw>>,
}

#[derive(Debug)]
pub(super) enum ConfigUpdateError {
    BadRequest(String),
    Unauthorized(String),
    Persistence(AppError),
}

pub(super) async fn prepare_and_commit_config_update(
    state: &Arc<AppState>,
    config_path: &Path,
    update: ConfigUpdate,
) -> Result<PreparedConfigUpdate, ConfigUpdateError> {
    let current = state.config.read().await.clone();
    let mut candidate = current.clone();
    let mut limits_changed = None;
    let mut server_changed = false;
    let mut deepseek_changed = false;
    let mut v7_changed = false;
    let mut new_password_hash = None;

    if let Some(limits) = update.limits.as_ref()
        && let Some(value) = limits.max_concurrent_requests
    {
        if value > MAX_CONCURRENT_REQUESTS {
            return Err(ConfigUpdateError::BadRequest(format!(
                "max_concurrent_requests must be between 0 and {MAX_CONCURRENT_REQUESTS}"
            )));
        }
        candidate.limits.max_concurrent_requests = value;
        limits_changed = Some(value);
    }

    if let Some(admin) = update.admin.as_ref()
        && let (Some(old_pw), Some(new_pw)) = (&admin.old_password, &admin.new_password)
    {
        if new_pw.is_empty() {
            return Err(ConfigUpdateError::Unauthorized(
                "New password is empty".to_string(),
            ));
        }
        let store = state.admin_store.read().await;
        if !store.verify_password(old_pw) {
            return Err(ConfigUpdateError::Unauthorized(
                "Old password is incorrect".to_string(),
            ));
        }
        let password_hash =
            crate::admin::auth::AdminStore::hash_password(new_pw).map_err(|_| {
                ConfigUpdateError::Persistence(AppError::Internal(
                    "Password hashing failed".to_string(),
                ))
            })?;
        let jwt_secret = store.jwt_secret.clone();
        drop(store);
        candidate.admin = Some(AdminConfig {
            jwt_secret: Some(jwt_secret),
            password_hash: Some(password_hash.clone()),
        });
        new_password_hash = Some(password_hash);
    }

    if let Some(server) = update.server.as_ref() {
        if let Some(host) = &server.host {
            candidate.server.host = host.clone();
            server_changed = true;
        }
        if let Some(port) = server.port {
            candidate.server.port = port;
            server_changed = true;
        }
        if let Some(cors_origins) = &server.cors_origins {
            tracing::info!(
                count = cors_origins.len(),
                "CORS origins received via admin UI"
            );
        }
    }

    if let Some(ouyi_update) = update.deepseek.as_ref() {
        if let Some(api_base) = &ouyi_update.api_base {
            candidate.ouyi.base_url = api_base.clone();
        }
        if let Some(jwt_token) = &ouyi_update.jwt_token
            && !jwt_token.trim().is_empty()
            && !is_masked_secret(jwt_token)
        {
            candidate.ouyi.jwt_token = jwt_token.clone();
        }
        if let Some(client_version) = &ouyi_update.client_version {
            candidate.ouyi.app_version = client_version.clone();
        }
        if let Some(interval) = ouyi_update.model_refresh_interval_secs {
            candidate.ouyi.model_refresh_interval_secs = interval;
        }
        deepseek_changed = true;
    }

    if let Some(v7_update) = update.ouyi_v7.as_ref() {
        let mut v7 = candidate.ouyi_v7.clone().unwrap_or(Ouyi7Config {
            base_url: "https://api-8.rcouyi.com".to_string(),
            chatapi_prefix: "/chatapi".to_string(),
            jwt_token: String::new(),
            default_model: "ouyi-chat".to_string(),
            model_refresh_interval_secs: 1800,
            max_history_messages: 20,
            simulated_stream_chunk_chars: 80,
        });
        if let Some(value) = &v7_update.base_url {
            v7.base_url = value.clone();
        }
        if let Some(value) = &v7_update.chatapi_prefix {
            v7.chatapi_prefix = value.clone();
        }
        if let Some(value) = &v7_update.jwt_token
            && !value.trim().is_empty()
            && !is_masked_secret(value)
        {
            v7.jwt_token = value.clone();
        }
        if let Some(value) = &v7_update.default_model {
            v7.default_model = value.clone();
        }
        if let Some(value) = v7_update.model_refresh_interval_secs {
            v7.model_refresh_interval_secs = value;
        }
        if let Some(value) = v7_update.max_history_messages {
            v7.max_history_messages = value;
        }
        if let Some(value) = v7_update.simulated_stream_chunk_chars {
            v7.simulated_stream_chunk_chars = value;
        }
        candidate.ouyi_v7 = Some(v7);
        v7_changed = true;
    }

    if let Some(platform_update) = update.platform.as_ref() {
        let mut platform = candidate.platform.clone().unwrap_or(PlatformConfig {
            default: crate::platform::OuyiPlatform::Ouyi8,
            prompt_on_start: true,
        });
        if let Some(default) = &platform_update.default {
            platform.default =
                crate::platform::OuyiPlatform::from_env_value(default).ok_or_else(|| {
                    ConfigUpdateError::BadRequest("Invalid platform default".to_string())
                })?;
        }
        if let Some(prompt_on_start) = platform_update.prompt_on_start {
            platform.prompt_on_start = prompt_on_start;
        }
        candidate.platform = Some(platform);
    }

    if let Some(proxy) = update.proxy.as_ref()
        && let Some(url) = proxy.url.as_ref()
    {
        tracing::info!(
            configured = !url.trim().is_empty(),
            "Proxy config received via admin UI"
        );
    }

    if let Some(tool_runtime_update) = update.tool_runtime.clone() {
        candidate.tool_runtime =
            build_tool_runtime_config(candidate.tool_runtime.as_ref(), tool_runtime_update);
    }
    if let Some(openai_tool_runtime_update) = update.openai_tool_runtime.clone() {
        candidate.openai_tool_runtime = build_openai_tool_runtime_config(
            candidate.openai_tool_runtime.as_ref(),
            openai_tool_runtime_update,
        );
    }

    let path = config_path.to_path_buf();
    let persisted = tokio::task::spawn_blocking(move || write_app_config(&path, &candidate))
        .await
        .map_err(|error| {
            ConfigUpdateError::Persistence(AppError::Internal(format!(
                "Config write task failed: {error}"
            )))
        })?
        .map_err(ConfigUpdateError::Persistence)?;

    {
        let mut state_config = state.config.write().await;
        *state_config = persisted.clone();
    }
    if let Some(value) = limits_changed {
        state.concurrency_limiter.set_limit(value);
    }
    if let Some(password_hash) = &new_password_hash {
        let mut store = state.admin_store.write().await;
        store.set_password_hash(password_hash.clone());
    }

    let active_platform = *state.platform.read().await;
    if deepseek_changed && active_platform == crate::platform::OuyiPlatform::Ouyi8 {
        let new_client = OuyiHttpClient::new(
            state.http_client.clone(),
            persisted.ouyi.base_url.clone(),
            persisted.ouyi.jwt_token.clone(),
            persisted.ouyi.app_version.clone(),
        );
        *state.ouyi_client.write().await = crate::client::OuyiClient::V8(new_client);
        state.platform_status.write().await.base_url = persisted.ouyi.base_url.clone();
    }
    if v7_changed
        && active_platform == crate::platform::OuyiPlatform::Ouyi7
        && let Some(v7) = persisted.ouyi_v7.clone()
    {
        let new_client = crate::client::ouyi_v7::Ouyi7HttpClient::new(
            state.http_client.clone(),
            v7.base_url.clone(),
            v7.chatapi_prefix.clone(),
            v7.jwt_token.clone(),
        );
        *state.ouyi_client.write().await = crate::client::OuyiClient::V7(new_client);
        state.platform_status.write().await.base_url = v7.base_url;
    }

    Ok(PreparedConfigUpdate {
        server_changed,
        accounts: update.accounts,
    })
}

pub async fn admin_put_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let update = match serde_json::from_value::<ConfigUpdate>(body) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false, "error": format!("Invalid config: {}", e)
                })),
            )
                .into_response();
        }
    };
    let _config_update_guard = CONFIG_UPDATE_LOCK.lock().await;

    let config_path = std::path::PathBuf::from(AppConfig::config_path());
    let prepared = match prepare_and_commit_config_update(&state, &config_path, update).await {
        Ok(prepared) => prepared,
        Err(ConfigUpdateError::BadRequest(error)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": error})),
            )
                .into_response();
        }
        Err(ConfigUpdateError::Unauthorized(error)) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"ok": false, "error": error})),
            )
                .into_response();
        }
        Err(ConfigUpdateError::Persistence(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": error.to_string()})),
            )
                .into_response();
        }
    };

    if let Some(raw_accounts) = prepared.accounts {
        let existing_accounts = state.account_store.get_accounts().await;
        let accounts = raw_accounts
            .into_iter()
            .map(|raw| {
                let existing = existing_accounts
                    .iter()
                    .find(|entry| entry.email == raw.email);
                build_account_entry_from_raw(raw, existing)
            })
            .collect();
        state.account_store.set_accounts(accounts).await;
    }

    let mut response = serde_json::json!({"ok": true});
    if prepared.server_changed {
        response["server_note"] = serde_json::Value::String(
            "Server host/port changes require a service restart".to_string(),
        );
    }
    (StatusCode::OK, Json(response)).into_response()
}

#[cfg(test)]
#[allow(dead_code)]
async fn legacy_admin_put_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let update = match serde_json::from_value::<ConfigUpdate>(body) {
        Ok(update) => update,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "error": format!("Invalid config: {error}")
                })),
            )
                .into_response();
        }
    };

    if let Some(limits) = update.limits
        && let Some(max_concurrent_requests) = limits.max_concurrent_requests
    {
        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        if let Err(error) =
            apply_limits_config_update(&state, &config_path, max_concurrent_requests).await
        {
            return match error {
                LimitsConfigUpdateError::Invalid(error) => (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"ok": false, "error": error})),
                )
                    .into_response(),
                LimitsConfigUpdateError::Persistence(error) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"ok": false, "error": error.to_string()})),
                )
                    .into_response(),
            };
        }
    }

    // Handle admin password change
    if let Some(admin) = update.admin
        && let Some(old_pw) = admin.old_password
        && let Some(new_pw) = admin.new_password
    {
        if new_pw.is_empty() {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "ok": false, "error": "New password is empty"
                })),
            )
                .into_response();
        }
        let mut store = state.admin_store.write().await;
        if !store.verify_password(&old_pw) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "ok": false, "error": "Old password is incorrect"
                })),
            )
                .into_response();
        }
        let password_hash = match crate::admin::auth::AdminStore::hash_password(&new_pw) {
            Ok(hash) => hash,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "ok": false, "error": "Password hashing failed"
                    })),
                )
                    .into_response();
            }
        };
        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        if let Err(e) = write_admin_config(&config_path, &store.jwt_secret, &password_hash) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "ok": false, "error": e.to_string()
                })),
            )
                .into_response();
        }
        store.set_password_hash(password_hash);
    }

    // Handle server config change (host/port)
    let mut server_changed = false;
    if let Some(server) = update.server {
        let mut config = state.config.read().await.clone();
        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        if let Some(host) = server.host {
            config.server.host = host;
            server_changed = true;
        }
        if let Some(port) = server.port {
            config.server.port = port;
            server_changed = true;
        }
        if let Some(cors_origins) = server.cors_origins {
            tracing::info!(
                count = cors_origins.len(),
                "CORS origins received via admin UI"
            );
        }
        if server_changed {
            if let Err(e) = write_server_config(&config_path, &config.server) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "ok": false, "error": e.to_string()
                    })),
                )
                    .into_response();
            }
            let mut current_config = state.config.write().await;
            *current_config = config;
            tracing::info!("Server config updated via admin UI (requires restart to take effect)");
        }
    }

    // Handle ouyi config change (api_base/jwt_token/client_version)
    if let Some(ouyi_update) = update.deepseek {
        let mut config = state.config.read().await.clone();
        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        if let Some(api_base) = ouyi_update.api_base {
            config.ouyi.base_url = api_base;
        }
        if let Some(jwt_token) = ouyi_update.jwt_token
            && !jwt_token.trim().is_empty()
            && !is_masked_secret(&jwt_token)
        {
            config.ouyi.jwt_token = jwt_token;
        }
        if let Some(client_version) = ouyi_update.client_version {
            config.ouyi.app_version = client_version;
        }
        if let Some(interval) = ouyi_update.model_refresh_interval_secs {
            config.ouyi.model_refresh_interval_secs = interval;
        }
        if let Err(e) = write_ouyi_config(&config_path, &config.ouyi) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "ok": false, "error": e.to_string()
                })),
            )
                .into_response();
        }
        let current_platform = *state.platform.read().await;
        {
            let mut current_config = state.config.write().await;
            *current_config = config.clone();
        }
        if current_platform == crate::platform::OuyiPlatform::Ouyi8 {
            let new_client = OuyiHttpClient::new(
                state.http_client.clone(),
                config.ouyi.base_url.clone(),
                config.ouyi.jwt_token.clone(),
                config.ouyi.app_version.clone(),
            );
            let mut ouyi_client = state.ouyi_client.write().await;
            *ouyi_client = crate::client::OuyiClient::V8(new_client);
            let mut platform_status = state.platform_status.write().await;
            platform_status.base_url = config.ouyi.base_url;
        }
        tracing::info!("Ouyi client config updated via admin UI");
    }

    // Handle Ouyi 7.0 config change
    if let Some(v7_update) = update.ouyi_v7 {
        let mut config = state.config.read().await.clone();
        let mut v7 = config.ouyi_v7.clone().unwrap_or(Ouyi7Config {
            base_url: "https://api-8.rcouyi.com".to_string(),
            chatapi_prefix: "/chatapi".to_string(),
            jwt_token: String::new(),
            default_model: "ouyi-chat".to_string(),
            model_refresh_interval_secs: 1800,
            max_history_messages: 20,
            simulated_stream_chunk_chars: 80,
        });
        if let Some(base_url) = v7_update.base_url {
            v7.base_url = base_url;
        }
        if let Some(prefix) = v7_update.chatapi_prefix {
            v7.chatapi_prefix = prefix;
        }
        if let Some(jwt_token) = v7_update.jwt_token
            && !jwt_token.trim().is_empty()
            && !is_masked_secret(&jwt_token)
        {
            v7.jwt_token = jwt_token;
        }
        if let Some(default_model) = v7_update.default_model {
            v7.default_model = default_model;
        }
        if let Some(interval) = v7_update.model_refresh_interval_secs {
            v7.model_refresh_interval_secs = interval;
        }
        if let Some(max_history) = v7_update.max_history_messages {
            v7.max_history_messages = max_history;
        }
        if let Some(chunk_chars) = v7_update.simulated_stream_chunk_chars {
            v7.simulated_stream_chunk_chars = chunk_chars;
        }

        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        if let Err(e) = write_ouyi_v7_config(&config_path, &v7) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "ok": false, "error": e.to_string()
                })),
            )
                .into_response();
        }

        config.ouyi_v7 = Some(v7.clone());
        let current_platform = *state.platform.read().await;
        {
            let mut current_config = state.config.write().await;
            *current_config = config;
        }
        if current_platform == crate::platform::OuyiPlatform::Ouyi7 {
            let new_client = crate::client::ouyi_v7::Ouyi7HttpClient::new(
                state.http_client.clone(),
                v7.base_url.clone(),
                v7.chatapi_prefix.clone(),
                v7.jwt_token.clone(),
            );
            let mut ouyi_client = state.ouyi_client.write().await;
            *ouyi_client = crate::client::OuyiClient::V7(new_client);
            let mut platform_status = state.platform_status.write().await;
            platform_status.base_url = v7.base_url;
        }
        tracing::info!("Ouyi V7 client config updated via admin UI");
    }

    // Handle platform startup selection
    if let Some(platform_update) = update.platform {
        let mut config = state.config.read().await.clone();
        let mut platform_config = config.platform.clone().unwrap_or(PlatformConfig {
            default: crate::platform::OuyiPlatform::Ouyi8,
            prompt_on_start: true,
        });
        if let Some(default) = platform_update.default {
            let Some(parsed) = crate::platform::OuyiPlatform::from_env_value(&default) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "ok": false, "error": "Invalid platform default"
                    })),
                )
                    .into_response();
            };
            platform_config.default = parsed;
        }
        if let Some(prompt_on_start) = platform_update.prompt_on_start {
            platform_config.prompt_on_start = prompt_on_start;
        }

        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        if let Err(e) = write_platform_config(&config_path, &platform_config) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "ok": false, "error": e.to_string()
                })),
            )
                .into_response();
        }
        config.platform = Some(platform_config);
        let mut current_config = state.config.write().await;
        *current_config = config;
        tracing::info!("Platform startup config updated via admin UI");
    }

    if let Some(proxy) = update.proxy
        && let Some(url) = proxy.url
    {
        tracing::info!(
            configured = !url.trim().is_empty(),
            "Proxy config received via admin UI"
        );
    }

    // Handle tool_runtime config change
    if let Some(tool_runtime_update) = update.tool_runtime {
        let mut config = state.config.read().await.clone();
        let next_tool_runtime =
            build_tool_runtime_config(config.tool_runtime.as_ref(), tool_runtime_update);
        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        match &next_tool_runtime {
            Some(runtime) => {
                if let Err(e) = write_tool_runtime_config(&config_path, runtime) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "ok": false, "error": e.to_string()
                        })),
                    )
                        .into_response();
                }
            }
            None => {
                if let Err(e) = remove_tool_runtime_config(&config_path) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "ok": false, "error": e.to_string()
                        })),
                    )
                        .into_response();
                }
            }
        }

        config.tool_runtime = next_tool_runtime;
        let mut current_config = state.config.write().await;
        *current_config = config;
        tracing::info!("Tool runtime config updated via admin UI");
    }

    // Handle openai_tool_runtime config change
    if let Some(openai_tool_runtime_update) = update.openai_tool_runtime {
        let mut config = state.config.read().await.clone();
        let next_openai_tool_runtime = build_openai_tool_runtime_config(
            config.openai_tool_runtime.as_ref(),
            openai_tool_runtime_update,
        );
        let config_path = std::path::PathBuf::from(AppConfig::config_path());
        match &next_openai_tool_runtime {
            Some(runtime) => {
                if let Err(e) = write_openai_tool_runtime_config(&config_path, runtime) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "ok": false, "error": e.to_string()
                        })),
                    )
                        .into_response();
                }
            }
            None => {
                if let Err(e) = remove_openai_tool_runtime_config(&config_path) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "ok": false, "error": e.to_string()
                        })),
                    )
                        .into_response();
                }
            }
        }

        config.openai_tool_runtime = next_openai_tool_runtime;
        let mut current_config = state.config.write().await;
        *current_config = config;
        tracing::info!("OpenAI tool runtime config updated via admin UI");
    }

    // Handle accounts
    if let Some(raw_accounts) = update.accounts {
        let existing_accounts = state.account_store.get_accounts().await;
        let accounts: Vec<AccountEntry> = raw_accounts
            .into_iter()
            .map(|r| {
                let existing = existing_accounts.iter().find(|a| a.email == r.email);
                build_account_entry_from_raw(r, existing)
            })
            .collect();
        state.account_store.set_accounts(accounts).await;
    }

    let mut resp = serde_json::json!({"ok": true});
    if server_changed {
        resp["server_note"] = serde_json::Value::String(
            "Server host/port changes require a service restart".to_string(),
        );
    }
    (StatusCode::OK, Json(resp)).into_response()
}

// ── Logs ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RequestLogResponse {
    timestamp: i64,
    request_id: String,
    model: String,
    api_key: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    latency_ms: u64,
    success: bool,
    platform: String,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    limit: Option<usize>,
}

pub async fn admin_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(100);
    let collector = &state.stats;
    let logs = collector.get_request_logs(limit);

    let response: Vec<RequestLogResponse> = logs
        .into_iter()
        .enumerate()
        .map(|(i, log)| RequestLogResponse {
            timestamp: log.timestamp.timestamp(),
            request_id: format!("req-{i:04x}"),
            model: log.model,
            api_key: "single".to_string(),
            prompt_tokens: log.prompt_tokens,
            completion_tokens: log.completion_tokens,
            latency_ms: log.latency_ms,
            success: log.error.is_none(),
            platform: log.platform,
        })
        .collect();

    Json(response)
}

// ── Runtime Logs ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RuntimeLogEntry {
    timestamp: String,
    level: String,
    target: String,
    message: String,
}

#[derive(Serialize)]
pub struct RuntimeLogsResponse {
    total: usize,
    offset: usize,
    limit: usize,
    logs: Vec<RuntimeLogEntry>,
}

#[derive(Deserialize)]
pub struct RuntimeLogsQuery {
    offset: Option<usize>,
    limit: Option<usize>,
}

pub async fn admin_runtime_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RuntimeLogsQuery>,
) -> impl IntoResponse {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(100);
    let collector = &state.stats;
    let all_logs = collector.get_runtime_logs(offset + limit);

    let total = all_logs.len();
    let page = all_logs.into_iter().skip(offset).take(limit);

    let entries: Vec<RuntimeLogEntry> = page
        .map(|log| RuntimeLogEntry {
            timestamp: log.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            level: log.level,
            target: "ouyi-api".to_string(),
            message: log.message,
        })
        .collect();

    Json(RuntimeLogsResponse {
        total,
        offset,
        limit,
        logs: entries,
    })
}

#[derive(Serialize)]
pub struct CategorizedModelsResponse {
    platform: String,
    categories: Vec<ModelCategory>,
}

pub async fn admin_categorized_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let categories = state.model_mapper.get_categorized_models();
    let platform_status = state.platform_status.read().await;
    let platform_str = serde_json::to_string(&platform_status.platform)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();
    Json(CategorizedModelsResponse {
        platform: platform_str,
        categories,
    })
}

// ── Tool Runtime Logs ────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ToolRuntimeLogResponse {
    timestamp: String,
    request_id: String,
    endpoint: String,
    mode: String,
    provider: String,
    model: String,
    base_url: String,
    decision: String,
    tool_names: Vec<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    latency_ms: u64,
    success: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
pub struct ToolRuntimeLogsQuery {
    limit: Option<usize>,
}

pub async fn admin_tool_runtime_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ToolRuntimeLogsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(100);
    let collector = &state.stats;
    let logs = collector.get_tool_runtime_logs(limit);

    let response: Vec<ToolRuntimeLogResponse> = logs
        .into_iter()
        .map(|log| ToolRuntimeLogResponse {
            timestamp: log.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            request_id: log.request_id,
            endpoint: log.endpoint,
            mode: log.mode,
            provider: log.provider,
            model: log.model,
            base_url: log.base_url,
            decision: log.decision,
            tool_names: log.tool_names,
            prompt_tokens: log.prompt_tokens,
            completion_tokens: log.completion_tokens,
            latency_ms: log.latency_ms,
            success: log.success,
            error: log.error,
        })
        .collect();

    Json(response)
}

pub async fn admin_tool_runtime_test(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ToolRuntimeTestRequest>,
) -> impl IntoResponse {
    let current_config = state.config.read().await.clone();
    let config = match build_tool_runtime_test_config(current_config.tool_runtime.as_ref(), body) {
        Some(config) => config,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ToolRuntimeTestResponse {
                    success: false,
                    message: "工具模型未启用".to_string(),
                    latency_ms: 0,
                    mode: "disabled".to_string(),
                    provider: "disabled".to_string(),
                    model: String::new(),
                    base_url: String::new(),
                }),
            )
                .into_response();
        }
    };

    let mode = "compiler".to_string();
    let provider = tool_runtime_provider_label(&config.provider).to_string();
    let model = config.model.clone().unwrap_or_default();
    let base_url = resolved_tool_runtime_base_url(&config).unwrap_or_default();

    if let Err(e) = validate_tool_runtime_test_config(&config) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ToolRuntimeTestResponse {
                success: false,
                message: e,
                latency_ms: 0,
                mode,
                provider,
                model,
                base_url,
            }),
        )
            .into_response();
    }

    let url = match tool_runtime_test_url(&config) {
        Ok(url) => url,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ToolRuntimeTestResponse {
                    success: false,
                    message: e,
                    latency_ms: 0,
                    mode,
                    provider,
                    model,
                    base_url,
                }),
            )
                .into_response();
        }
    };
    let api_key = match config.get_api_key() {
        Ok(api_key) => api_key,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ToolRuntimeTestResponse {
                    success: false,
                    message: "tool_runtime.api_key 未配置".to_string(),
                    latency_ms: 0,
                    mode,
                    provider,
                    model,
                    base_url,
                }),
            )
                .into_response();
        }
    };

    let started = Instant::now();
    let result = match &config.provider {
        ToolRuntimeProvider::OpenaiCompatible => {
            send_openai_tool_runtime_test(&state, &config, &url, &api_key).await
        }
        ToolRuntimeProvider::Anthropic => {
            send_anthropic_tool_runtime_test(&state, &config, &url, &api_key).await
        }
    };
    let latency_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(()) => Json(ToolRuntimeTestResponse {
            success: true,
            message: "工具模型连接测试通过".to_string(),
            latency_ms,
            mode,
            provider,
            model,
            base_url,
        })
        .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(ToolRuntimeTestResponse {
                success: false,
                message: e,
                latency_ms,
                mode,
                provider,
                model,
                base_url,
            }),
        )
            .into_response(),
    }
}

pub async fn admin_openai_tool_runtime_test(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ToolRuntimeTestRequest>,
) -> impl IntoResponse {
    let current_config = state.config.read().await.clone();
    let config = match build_openai_tool_runtime_config(
        current_config.openai_tool_runtime.as_ref(),
        ToolRuntimeConfigUpdate {
            enabled: body.enabled,
            provider: body.provider,
            base_url: body.base_url,
            api_key: body.api_key,
            model: body.model,
            timeout_secs: body.timeout_secs,
        },
    ) {
        Some(config) => config,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ToolRuntimeTestResponse {
                    success: false,
                    message: "OpenAI 工具模型未启用".to_string(),
                    latency_ms: 0,
                    mode: "disabled".to_string(),
                    provider: "openai-compatible".to_string(),
                    model: String::new(),
                    base_url: String::new(),
                }),
            )
                .into_response();
        }
    };

    let mode = "compiler".to_string();
    let provider = "openai-compatible".to_string();
    let model = config.model.clone().unwrap_or_default();
    let base_url = resolved_tool_runtime_base_url(&config).unwrap_or_default();

    if let Err(e) = validate_tool_runtime_test_config(&config) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ToolRuntimeTestResponse {
                success: false,
                message: e,
                latency_ms: 0,
                mode,
                provider,
                model,
                base_url,
            }),
        )
            .into_response();
    }

    let url = match tool_runtime_test_url(&config) {
        Ok(url) => url,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ToolRuntimeTestResponse {
                    success: false,
                    message: e,
                    latency_ms: 0,
                    mode,
                    provider,
                    model,
                    base_url,
                }),
            )
                .into_response();
        }
    };
    let api_key = match config.get_api_key() {
        Ok(api_key) => api_key,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ToolRuntimeTestResponse {
                    success: false,
                    message: "openai_tool_runtime.api_key 未配置".to_string(),
                    latency_ms: 0,
                    mode,
                    provider,
                    model,
                    base_url,
                }),
            )
                .into_response();
        }
    };

    let started = Instant::now();
    let result = send_openai_tool_runtime_test(&state, &config, &url, &api_key).await;
    let latency_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(()) => Json(ToolRuntimeTestResponse {
            success: true,
            message: "OpenAI 工具模型连接测试通过".to_string(),
            latency_ms,
            mode,
            provider,
            model,
            base_url,
        })
        .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(ToolRuntimeTestResponse {
                success: false,
                message: e,
                latency_ms,
                mode,
                provider,
                model,
                base_url,
            }),
        )
            .into_response(),
    }
}

// ── Accounts ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AccountView {
    pub email: String,
    pub mobile: String,
    pub area_code: String,
    pub token: Option<String>,
    pub enabled: bool,
    pub state: String,
    pub request_count: u64,
}

pub async fn admin_list_accounts(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let accounts = state.account_store.get_accounts().await;
    let runtime = state.account_store.get_runtime_stats().await;

    let views: Vec<AccountView> = accounts
        .iter()
        .map(|a| {
            let rt = runtime.iter().find(|(email, _)| email == &a.email);
            let (state_str, req_count) = rt
                .map(|(_, rt)| {
                    let s = match rt.state {
                        AccountState::Idle => "idle".to_string(),
                        AccountState::Busy => "busy".to_string(),
                        AccountState::Error { count } => format!("error:{}", count),
                        AccountState::Invalid => "invalid".to_string(),
                    };
                    (s, rt.request_count)
                })
                .unwrap_or_else(|| ("unknown".to_string(), 0));

            AccountView {
                email: a.email.clone(),
                mobile: a.mobile.clone(),
                area_code: a.area_code.clone(),
                token: a.token.as_ref().map(|t| mask_account_token(t)),
                enabled: a.enabled,
                state: state_str,
                request_count: req_count,
            }
        })
        .collect();

    Json(views)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_runtime_config(
        mode: ToolRuntimeMode,
        provider: ToolRuntimeProvider,
    ) -> ToolRuntimeConfig {
        ToolRuntimeConfig {
            enabled: true,
            mode,
            provider,
            base_url: Some("https://tool.example.com/v1/".to_string()),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            model: Some("tool-model".to_string()),
            timeout_secs: 30,
        }
    }

    #[test]
    fn compiler_openai_test_url_uses_chat_completions() {
        let config = tool_runtime_config(
            ToolRuntimeMode::Compiler,
            ToolRuntimeProvider::OpenaiCompatible,
        );

        assert_eq!(
            tool_runtime_test_url(&config).unwrap(),
            "https://tool.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn anthropic_test_url_uses_messages() {
        let config = tool_runtime_config(ToolRuntimeMode::Compiler, ToolRuntimeProvider::Anthropic);

        assert_eq!(
            tool_runtime_test_url(&config).unwrap(),
            "https://tool.example.com/v1/messages"
        );
    }

    #[test]
    fn test_config_accepts_anthropic_and_openai_providers() {
        let config = tool_runtime_config(ToolRuntimeMode::Compiler, ToolRuntimeProvider::Anthropic);

        assert!(validate_tool_runtime_test_config(&config).is_ok());

        let config = tool_runtime_config(
            ToolRuntimeMode::Compiler,
            ToolRuntimeProvider::OpenaiCompatible,
        );
        assert!(validate_tool_runtime_test_config(&config).is_ok());
    }

    #[test]
    fn test_config_requires_model_and_api_key() {
        let mut config = tool_runtime_config(
            ToolRuntimeMode::Compiler,
            ToolRuntimeProvider::OpenaiCompatible,
        );
        config.model = None;
        assert!(
            validate_tool_runtime_test_config(&config)
                .unwrap_err()
                .contains("model")
        );

        config.model = Some("tool-model".to_string());
        config.api_key = None;
        assert!(
            validate_tool_runtime_test_config(&config)
                .unwrap_err()
                .contains("api_key")
        );
    }

    #[test]
    fn disabled_test_request_removes_runtime_config() {
        let current = tool_runtime_config(
            ToolRuntimeMode::Compiler,
            ToolRuntimeProvider::OpenaiCompatible,
        );

        let next = build_tool_runtime_test_config(
            Some(&current),
            ToolRuntimeTestRequest {
                enabled: Some(false),
                provider: None,
                base_url: None,
                api_key: None,
                model: None,
                timeout_secs: None,
            },
        );

        assert!(next.is_none());
    }

    #[test]
    fn account_token_view_is_masked() {
        assert_eq!(
            mask_account_token("jwt-token-1234567890"),
            "jwt-toke...34567890"
        );
    }

    #[test]
    fn masked_account_update_preserves_existing_token() {
        let existing = AccountEntry {
            email: "a@example.com".to_string(),
            mobile: "13800000000".to_string(),
            area_code: "86".to_string(),
            password: String::new(),
            token: Some("jwt-token-1234567890".to_string()),
            enabled: true,
        };
        let raw = AccountEntryRaw {
            email: "a@example.com".to_string(),
            mobile: "13800000001".to_string(),
            area_code: "86".to_string(),
            password: String::new(),
            token: Some("jwt-toke...34567890".to_string()),
            enabled: false,
        };

        let merged = build_account_entry_from_raw(raw, Some(&existing));

        assert_eq!(merged.token, existing.token);
        assert_eq!(merged.mobile, "13800000001");
        assert!(!merged.enabled);
    }
}

use crate::admin::accounts::AccountState;

#[derive(Deserialize)]
pub struct AddAccountRequest {
    pub email: String,
    pub mobile: String,
    #[serde(default)]
    pub area_code: String,
    #[serde(default)]
    pub password: String,
    pub token: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

pub async fn admin_add_account(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddAccountRequest>,
) -> impl IntoResponse {
    let password = if req.password.is_empty() {
        String::new()
    } else {
        hash(&req.password, DEFAULT_COST).unwrap_or_default()
    };
    let new_entry = AccountEntry {
        email: req.email,
        mobile: req.mobile,
        area_code: req.area_code,
        password,
        token: req.token.filter(|t| !t.is_empty()),
        enabled: req.enabled,
    };

    match state.account_store.add_account(new_entry).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "ok": false, "error": e
            })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct RemoveAccountRequest {
    pub email: String,
}

pub async fn admin_remove_account(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RemoveAccountRequest>,
) -> impl IntoResponse {
    let mut accounts = state.account_store.get_accounts().await;
    let len_before = accounts.len();
    accounts.retain(|a| a.email != req.email);

    if accounts.len() == len_before {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "ok": false, "error": "Account not found"
            })),
        )
            .into_response();
    }

    state.account_store.set_accounts(accounts).await;
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

pub async fn admin_refresh_token(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let accounts = state.account_store.get_accounts().await;
    let tokens: Vec<String> = accounts
        .into_iter()
        .filter_map(|a| a.token.map(|t| mask_account_token(&t)))
        .collect();

    Json(serde_json::json!({
        "tokens": tokens,
        "count": tokens.len()
    }))
}
