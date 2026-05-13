use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use reqwest::StatusCode as ReqwestStatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::AppState;
use crate::admin::accounts::AccountEntry;
use crate::client::http::OuyiHttpClient;
use crate::config::{
    AppConfig, ToolRuntimeConfig, ToolRuntimeMode, ToolRuntimeProvider, remove_tool_runtime_config,
    write_tool_runtime_config,
};
use crate::handler::models::list_models as list_openai_models;
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
    store.setup(&req.password);
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

    Json(AdminStatusResponse {
        accounts: account_values,
        total: stats.total,
        idle: stats.idle,
        busy: stats.busy,
        error: stats.error,
        invalid: stats.invalid,
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
pub struct FullConfigResponse {
    server: ServerConfigView,
    deepseek: DeepSeekConfigView,
    proxy: ProxyConfigView,
    admin: AdminConfigResponse,
    accounts: Vec<serde_json::Value>,
    api_keys: Vec<serde_json::Value>,
    tool_runtime: ToolRuntimeConfigView,
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
                "token": a.token.as_deref().unwrap_or(""),
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

    Json(FullConfigResponse {
        server: ServerConfigView {
            host: config.server.host.clone(),
            port: config.server.port,
            cors_origins: vec!["*".to_string()],
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

#[derive(Deserialize)]
pub struct ConfigUpdate {
    pub server: Option<ServerConfigUpdate>,
    pub deepseek: Option<OuyiConfigUpdate>,
    pub proxy: Option<ProxyConfigUpdate>,
    pub admin: Option<AdminUpdate>,
    pub accounts: Option<Vec<AccountEntryRaw>>,
    pub tool_runtime: Option<ToolRuntimeConfigUpdate>,
}

#[derive(Deserialize)]
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

#[derive(Deserialize)]
pub struct ServerConfigUpdate {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub cors_origins: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct OuyiConfigUpdate {
    pub api_base: Option<String>,
    pub client_version: Option<String>,
    /// 前端传来的 jwt_token 字段（对应后端 ouyi.jwt_token）
    pub jwt_token: Option<String>,
}

#[derive(Deserialize)]
pub struct ProxyConfigUpdate {
    pub url: Option<String>,
}

#[derive(Deserialize)]
pub struct AdminUpdate {
    pub old_password: Option<String>,
    pub new_password: Option<String>,
}

#[derive(Deserialize)]
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
        store.setup(&new_pw);
    }

    // Handle server config change (host/port)
    let mut server_changed = false;
    if let Some(server) = update.server {
        let mut config = state.config.read().await.clone();
        if let Some(host) = server.host {
            config.server.host = host;
            server_changed = true;
        }
        if let Some(port) = server.port {
            config.server.port = port;
            server_changed = true;
        }
        if server_changed {
            tracing::info!("Server config updated via admin UI (requires restart to take effect)");
        }
    }

    // Handle ouyi config change (api_base/jwt_token/client_version)
    if let Some(ouyi_update) = update.deepseek {
        let mut config = state.config.read().await.clone();
        if let Some(api_base) = ouyi_update.api_base {
            config.ouyi.base_url = api_base;
        }
        if let Some(jwt_token) = ouyi_update.jwt_token {
            config.ouyi.jwt_token = jwt_token;
        }
        if let Some(client_version) = ouyi_update.client_version {
            config.ouyi.app_version = client_version;
        }
        // Rebuild the ouyi client with new config
        let new_client = OuyiHttpClient::new(
            state.http_client.clone(),
            config.ouyi.base_url.clone(),
            config.ouyi.jwt_token.clone(),
            config.ouyi.app_version.clone(),
        );
        {
            let mut current_config = state.config.write().await;
            *current_config = config;
        }
        let mut ouyi_client = state.ouyi_client.write().await;
        *ouyi_client = new_client;
        tracing::info!("Ouyi client config updated via admin UI");
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

    // Handle accounts
    if let Some(raw_accounts) = update.accounts {
        let accounts: Vec<AccountEntry> = raw_accounts
            .into_iter()
            .map(|r| {
                let password = if r.password.is_empty() {
                    String::new()
                } else {
                    hash(&r.password, DEFAULT_COST).unwrap_or_default()
                };
                AccountEntry {
                    email: r.email,
                    mobile: r.mobile,
                    area_code: r.area_code,
                    password,
                    token: r.token.filter(|t| !t.is_empty()),
                    enabled: r.enabled,
                }
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

pub async fn admin_categorized_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let categories = state.model_mapper.get_categorized_models();
    Json(categories)
}

// ── Tool Runtime Logs ────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ToolRuntimeLogResponse {
    timestamp: String,
    request_id: String,
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
                token: a.token.as_ref().map(|t| {
                    if t.len() > 16 {
                        format!("{}...{}", &t[..8], &t[t.len() - 8..])
                    } else {
                        t.clone()
                    }
                }),
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
    let tokens: Vec<String> = accounts.into_iter().filter_map(|a| a.token).collect();

    Json(serde_json::json!({
        "tokens": tokens,
        "count": tokens.len()
    }))
}
