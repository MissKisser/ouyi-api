use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::handler::models::list_models as list_openai_models;
use crate::admin::accounts::AccountEntry;
use crate::client::http::OuyiHttpClient;
use bcrypt::{hash, DEFAULT_COST};
use crate::AppState;

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
        return Err((StatusCode::CONFLICT, Json(serde_json::json!({
            "error": "Setup already completed"
        }))));
    }
    store.setup(&req.password);
    let token = match store.generate_token("admin") {
        Ok(t) => t,
        Err(_) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Token generation failed"
            }))));
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
        return Err((StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
            "error": "Too many login attempts, please try again later"
        }))));
    }

    if !store.verify_password(&req.password) {
        return Err((StatusCode::UNAUTHORIZED, Json(serde_json::json!({
            "error": "Invalid password"
        }))));
    }

    let token = match store.generate_token("admin") {
        Ok(t) => t,
        Err(_) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Token generation failed"
            }))));
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
        let entry = models_map.entry(log.model.clone()).or_insert(ModelStatsEntry {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 0,
        });
        entry.requests += 1;
        entry.prompt_tokens += log.prompt_tokens;
        entry.completion_tokens += log.completion_tokens;
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
pub struct FullConfigResponse {
    server: ServerConfigView,
    deepseek: DeepSeekConfigView,
    proxy: ProxyConfigView,
    admin: AdminConfigResponse,
    accounts: Vec<serde_json::Value>,
    api_keys: Vec<serde_json::Value>,
}

pub async fn admin_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store = state.admin_store.read().await;
    let config = &state.config;
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
    })
}

#[derive(Deserialize)]
pub struct ConfigUpdate {
    pub server: Option<ServerConfigUpdate>,
    pub deepseek: Option<OuyiConfigUpdate>,
    pub proxy: Option<ProxyConfigUpdate>,
    pub admin: Option<AdminUpdate>,
    pub accounts: Option<Vec<AccountEntryRaw>>,
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
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "ok": false, "error": format!("Invalid config: {}", e)
            }))).into_response();
        }
    };

    // Handle admin password change
    if let Some(admin) = update.admin
        && let Some(old_pw) = admin.old_password
        && let Some(new_pw) = admin.new_password {
        if new_pw.is_empty() {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({
                "ok": false, "error": "New password is empty"
            }))).into_response();
        }
        let mut store = state.admin_store.write().await;
        if !store.verify_password(&old_pw) {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({
                "ok": false, "error": "Old password is incorrect"
            }))).into_response();
        }
        store.setup(&new_pw);
    }

    // Handle server config change (host/port)
    let mut server_changed = false;
    if let Some(server) = update.server {
        let mut config = state.config.clone();
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
        let mut config = state.config.clone();
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
        let mut ouyi_client = state.ouyi_client.write().await;
        *ouyi_client = new_client;
        tracing::info!("Ouyi client config updated via admin UI");
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
            "Server host/port changes require a service restart".to_string()
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
            let (state_str, req_count) = rt.map(|(_, rt)| {
                let s = match rt.state {
                    AccountState::Idle => "idle".to_string(),
                    AccountState::Busy => "busy".to_string(),
                    AccountState::Error { count } => format!("error:{}", count),
                    AccountState::Invalid => "invalid".to_string(),
                };
                (s, rt.request_count)
            }).unwrap_or_else(|| ("unknown".to_string(), 0));

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
        Err(e) => (StatusCode::CONFLICT, Json(serde_json::json!({
            "ok": false, "error": e
        }))).into_response(),
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
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "ok": false, "error": "Account not found"
        }))).into_response();
    }

    state.account_store.set_accounts(accounts).await;
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

pub async fn admin_refresh_token(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let accounts = state.account_store.get_accounts().await;
    let tokens: Vec<String> = accounts
        .into_iter()
        .filter_map(|a| a.token)
        .collect();

    Json(serde_json::json!({
        "tokens": tokens,
        "count": tokens.len()
    }))
}
