//! 管理后台 HTTP 处理器：初始化与登录、状态与统计查询、配置读写、
//! 模型列表、请求/运行时/工具运行时日志、工具模型连通性测试，
//! 以及账号池增删查接口。
//!
//! 除 `admin_setup` 与 `admin_login` 外，其余路由统一由 `super::jwt_middleware`
//! 保护；配置写路径共享 [`CONFIG_UPDATE_LOCK`] 以串行化并发修改。

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
    AdminConfig, ApiKeysConfig, AppConfig, Ouyi7Config, PlatformConfig, ToolRuntimeConfig,
    ToolRuntimeMode, ToolRuntimeProvider, write_admin_config, write_app_config,
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

/// 初始化设置 / 登录请求体。
#[derive(Deserialize)]
pub struct LoginRequest {
    /// 管理员明文密码。
    password: String,
}

/// 初始化设置 / 登录成功响应体。
#[derive(Serialize)]
pub struct LoginResponse {
    /// 后续请求应携带的 Bearer JWT。
    token: String,
}

/// 首次初始化接口：为管理后台设置密码。
///
/// 仅在未完成初始化时可用；成功后把密码哈希持久化到 admin 配置文件并
/// 签发登录 token。副作用：写配置文件、更新内存中的密码哈希。
///
/// 失败时返回：409（已完成初始化）、500（密码哈希失败 / token 生成失败 /
/// 配置写盘失败，附错误信息）。
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

/// 管理后台登录接口：校验密码并签发 JWT。
///
/// 按客户端 IP 做登录限流。失败时返回：403（未完成初始化）、429（触发
/// 限流）、401（密码错误）、500（token 生成失败）。
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

/// `GET /admin/api/status` 响应体：账号池与当前活动平台的整体状态。
#[derive(Serialize)]
pub struct AdminStatusResponse {
    /// 各账号的摘要（邮箱、启用、是否持有 token、运行状态）。
    accounts: Vec<serde_json::Value>,
    /// 账号总数。
    total: usize,
    /// 空闲账号数。
    idle: usize,
    /// 忙碌账号数。
    busy: usize,
    /// 出错账号数。
    error: usize,
    /// 失效账号数。
    invalid: usize,
    /// 当前活动平台的状态视图。
    platform: PlatformStatusView,
}

/// 当前活动平台的状态视图。
#[derive(Serialize)]
pub struct PlatformStatusView {
    /// 平台标识（如 "ouyi7" / "ouyi8"）。
    platform: String,
    /// 平台展示名（如 "Ouyi 7.0"）。
    label: String,
    /// 平台上游服务地址。
    base_url: String,
    /// 当前已加载的可用模型数量。
    model_count: usize,
    /// 上游凭据是否经最近一次校验有效。
    token_valid: bool,
    /// 最近一次平台选择时间的 RFC3339 字符串。
    last_selected_at: Option<String>,
}

/// 查询账号池状态与当前活动平台信息。只读，无副作用。
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

/// `GET /admin/api/stats` 响应体：全服务请求统计快照。
#[derive(Serialize)]
pub struct StatsSnapshot {
    /// 总请求数。
    total_requests: u64,
    /// 成功请求数。
    success_requests: u64,
    /// 失败请求数。
    failed_requests: u64,
    /// 平均延迟（毫秒）；尚无请求时为 0。
    avg_latency_ms: u64,
    /// 累计消耗的 prompt token 数（含工具模型调用）。
    total_prompt_tokens: u64,
    /// 累计产出的 completion token 数（含工具模型调用）。
    total_completion_tokens: u64,
    /// 服务运行时长（秒）。
    uptime_secs: u64,
    /// 按模型聚合的用量统计；工具模型以 `tool-runtime::<model>` 为键。
    models: HashMap<String, ModelStatsEntry>,
    /// 按 API key 聚合的用量统计（当前未启用，恒为空表）。
    keys: HashMap<String, KeyStatsEntry>,
}

/// 单个模型的用量统计条目。
#[derive(Serialize)]
pub struct ModelStatsEntry {
    /// 该模型累计消耗的 prompt token。
    prompt_tokens: u64,
    /// 该模型累计产出的 completion token。
    completion_tokens: u64,
    /// 该模型的请求次数。
    requests: u64,
}

/// 单个 API key 的用量统计条目。
#[derive(Serialize)]
pub struct KeyStatsEntry {
    /// 该 key 累计消耗的 prompt token。
    prompt_tokens: u64,
    /// 该 key 累计产出的 completion token。
    completion_tokens: u64,
    /// 该 key 的请求次数。
    requests: u64,
}

/// 汇总全服务请求统计并按模型聚合。只读，无副作用。
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

/// 转发到 OpenAI 兼容的模型列表接口，返回当前平台可用模型。
pub async fn admin_models(state: State<Arc<AppState>>) -> impl IntoResponse {
    list_openai_models(state).await.into_response()
}

// ── Config ────────────────────────────────────────────────────────────────

/// 管理员凭据配置视图。
#[derive(Serialize)]
pub struct AdminConfigResponse {
    /// 是否已设置管理员密码（即已完成初始化）。
    password_set: bool,
    /// 当前登录 JWT 的签发时间戳（当前未跟踪，恒为 0）。
    jwt_issued_at: u64,
}

/// 工具调用标记扩展配置视图。
#[derive(Serialize)]
pub struct ToolCallTagConfig {
    /// 额外识别为工具调用开始的标记串（当前恒为空表）。
    extra_starts: Vec<String>,
    /// 额外识别为工具调用结束的标记串（当前恒为空表）。
    extra_ends: Vec<String>,
}

/// ouyi 8.0 上游配置视图，对应配置文件 `[ouyi]` 段。
#[derive(Serialize)]
pub struct DeepSeekConfigView {
    /// 上游服务地址。
    api_base: String,
    /// 预留字段，当前恒为空串。
    wasm_url: String,
    /// 预留字段，当前恒为空串。
    user_agent: String,
    /// 客户端版本标识（对应 `ouyi.app_version`）。
    client_version: String,
    /// 预留字段，当前恒为空串。
    client_platform: String,
    /// 预留字段，当前恒为空串。
    client_locale: String,
    /// 预留字段，当前恒为空表。
    model_types: Vec<String>,
    /// 预留字段，当前恒为空表。
    max_input_tokens: Vec<u32>,
    /// 预留字段，当前恒为空表。
    max_output_tokens: Vec<u32>,
    /// 预留字段，当前恒为空表。
    model_aliases: Vec<String>,
    /// 工具调用标记扩展配置。
    tool_call: ToolCallTagConfig,
    /// JWT token for ouyi API auth（对应后端 ouyi.jwt_token），返回时已脱敏。
    jwt_token: String,
    /// 模型列表后台刷新间隔（秒）。
    model_refresh_interval_secs: u64,
}

/// 服务监听配置视图。
#[derive(Serialize)]
pub struct ServerConfigView {
    /// 监听地址。
    host: String,
    /// 监听端口。
    port: u16,
    /// 允许的 CORS 来源列表。
    cors_origins: Vec<String>,
}

/// 代理配置视图。
#[derive(Serialize)]
pub struct ProxyConfigView {
    /// 出站代理地址；未配置时为 `None`。
    url: Option<String>,
}

/// 工具运行时配置视图（anthropic 工具模型与 OpenAI 工具模型共用结构）。
#[derive(Serialize)]
pub struct ToolRuntimeConfigView {
    /// 是否启用。
    enabled: bool,
    /// 工具模型提供方（"anthropic" / "openai-compatible"）；未启用时为 `None`。
    provider: Option<String>,
    /// 工具模型服务地址；未配置时为 `None`。
    base_url: Option<String>,
    /// API key（返回时已脱敏）；未配置时为 `None`。
    api_key: Option<String>,
    /// 工具模型名；未配置时为 `None`。
    model: Option<String>,
    /// 请求超时（秒）。
    timeout_secs: u64,
}

/// 平台配置视图。
#[derive(Serialize)]
pub struct PlatformConfigView {
    /// 启动默认平台标识。
    default: String,
    /// 启动时是否提示确认平台选择。
    prompt_on_start: bool,
}

/// ouyi 7.0 上游配置视图，对应配置文件 `[ouyi_v7]` 段。
#[derive(Serialize)]
pub struct Ouyi7ConfigView {
    /// 上游服务根地址。
    base_url: String,
    /// 聊天 API 路径前缀（如 "/chatapi"）。
    chatapi_prefix: String,
    /// JWT 凭据（返回时已脱敏）。
    jwt_token: String,
    /// 默认模型名。
    default_model: String,
    /// 模型列表后台刷新间隔（秒）。
    model_refresh_interval_secs: u64,
    /// 发送消息时携带的最大历史消息条数。
    max_history_messages: usize,
    /// 模拟流式输出的每个分块字符数。
    simulated_stream_chunk_chars: usize,
}

/// 并发限制配置视图。
#[derive(Serialize)]
pub struct LimitsConfigView {
    /// 最大并发请求数；0 表示不限制。
    max_concurrent_requests: usize,
}

/// `GET /admin/api/config` 响应体：完整配置视图的聚合。
#[derive(Serialize)]
pub struct FullConfigResponse {
    /// 服务监听配置。
    server: ServerConfigView,
    /// 并发限制配置。
    limits: LimitsConfigView,
    /// ouyi 8.0 上游配置。
    deepseek: DeepSeekConfigView,
    /// 代理配置。
    proxy: ProxyConfigView,
    /// 管理员凭据状态。
    admin: AdminConfigResponse,
    /// 账号列表摘要（token 已脱敏、密码不回传）。
    accounts: Vec<serde_json::Value>,
    /// API key 掩码列表（保留首尾少量字符；未配置时为空表）。
    api_keys: Vec<serde_json::Value>,
    /// anthropic 工具运行时配置。
    tool_runtime: ToolRuntimeConfigView,
    /// OpenAI 工具运行时配置。
    openai_tool_runtime: ToolRuntimeConfigView,
    /// 平台配置；配置文件无 `[platform]` 段时以当前活动平台兜底填充。
    platform: Option<PlatformConfigView>,
    /// ouyi 7.0 上游配置；未配置时为 `None`。
    ouyi_v7: Option<Ouyi7ConfigView>,
}

/// 查询完整配置视图。只读，无副作用；敏感字段（JWT/API key）脱敏返回。
pub async fn admin_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store = state.admin_store.read().await;
    let config = state.config.read().await.clone();
    // 读 current active platform 作兜底：如果 config.toml 没有 [platform] 段
    // （config.platform = None），用 state.platform 保证返回的 platform.default
    // 总与"当前实际活动平台"一致，避免前端 persistedPlatformDefault=null 导致
    // 平台切换按钮永远灰色。
    let active_platform = *state.platform.read().await;
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

    let api_keys: Vec<serde_json::Value> = config
        .api_keys
        .as_ref()
        .map(|section| {
            section
                .keys
                .iter()
                .map(|key| serde_json::json!({ "masked": crate::api_auth::mask_key(key) }))
                .collect()
        })
        .unwrap_or_default();

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
        api_keys,
        tool_runtime: tool_runtime_view,
        openai_tool_runtime: openai_tool_runtime_view,
        platform: Some({
            let p = config.platform.as_ref();
            let default = p.map(|p| p.default).unwrap_or(active_platform);
            let prompt_on_start = p.map(|p| p.prompt_on_start).unwrap_or(true);
            PlatformConfigView {
                default: serde_json::to_string(&default)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string(),
                prompt_on_start,
            }
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

/// `PUT /admin/api/config` 请求体：各配置段的可选更新，`None` 表示不修改。
#[derive(Clone, Deserialize)]
pub struct ConfigUpdate {
    /// 服务监听配置更新。
    pub server: Option<ServerConfigUpdate>,
    /// 并发限制更新。
    pub limits: Option<LimitsConfigUpdate>,
    /// ouyi 8.0 上游配置更新（字段名沿用 `deepseek`）。
    pub deepseek: Option<OuyiConfigUpdate>,
    /// 代理配置更新。
    pub proxy: Option<ProxyConfigUpdate>,
    /// 管理员密码更新。
    pub admin: Option<AdminUpdate>,
    /// 账号池整体替换列表。
    pub accounts: Option<Vec<AccountEntryRaw>>,
    /// anthropic 工具运行时配置更新。
    pub tool_runtime: Option<ToolRuntimeConfigUpdate>,
    /// OpenAI 工具运行时配置更新。
    pub openai_tool_runtime: Option<ToolRuntimeConfigUpdate>,
    /// 平台配置更新。
    pub platform: Option<PlatformConfigUpdate>,
    /// ouyi 7.0 上游配置更新。
    pub ouyi_v7: Option<Ouyi7ConfigUpdate>,
    /// API Key 鉴权更新。
    pub api_keys: Option<ApiKeysConfigUpdate>,
}

/// 并发限制更新段。
#[derive(Clone, Deserialize)]
pub struct LimitsConfigUpdate {
    /// 新的最大并发请求数；`None` 表示不修改。
    pub max_concurrent_requests: Option<usize>,
}

/// API Key 鉴权更新段。
#[derive(Clone, Deserialize)]
pub struct ApiKeysConfigUpdate {
    /// 新的 key 列表，按以下语义处理：
    /// - `None`：不修改。
    /// - 空数组：显式关闭鉴权并清空 key。
    /// - 数组全部为空白或掩码占位项：视为未提供真实 key，保留现有 api_keys 不变。
    /// - 含至少一个真实 key（≥16 字符）：空白与掩码项丢弃，真实项去重后整体替换。
    pub keys: Option<Vec<String>>,
}

/// 工具运行时配置更新段（两种提供方共用结构）。
#[derive(Clone, Deserialize)]
pub struct ToolRuntimeConfigUpdate {
    /// 是否启用；显式传 `false` 时移除该运行时配置。
    pub enabled: Option<bool>,
    /// 提供方标识（"anthropic" / "openai-compatible"）。
    pub provider: Option<String>,
    /// 工具模型服务地址。
    pub base_url: Option<String>,
    /// API key；脱敏占位值会被忽略以保留原值。
    pub api_key: Option<String>,
    /// 工具模型名。
    pub model: Option<String>,
    /// 请求超时（秒）。
    pub timeout_secs: Option<u64>,
}

/// 工具模型连通性测试请求体（字段语义同 [`ToolRuntimeConfigUpdate`]，
/// 仅作用于本次测试，不落盘）。
#[derive(Deserialize)]
pub struct ToolRuntimeTestRequest {
    /// 是否启用（不启用时测试直接拒绝）。
    pub enabled: Option<bool>,
    /// 提供方标识。
    pub provider: Option<String>,
    /// 工具模型服务地址。
    pub base_url: Option<String>,
    /// API key。
    pub api_key: Option<String>,
    /// 工具模型名。
    pub model: Option<String>,
    /// 请求超时（秒）。
    pub timeout_secs: Option<u64>,
}

/// 工具模型连通性测试响应体。
#[derive(Serialize)]
pub struct ToolRuntimeTestResponse {
    /// 测试是否通过。
    pub success: bool,
    /// 结果描述或失败原因。
    pub message: String,
    /// 测试请求耗时（毫秒）。
    pub latency_ms: u64,
    /// 运行模式（"compiler"，未启用时为 "disabled"）。
    pub mode: String,
    /// 提供方标识。
    pub provider: String,
    /// 参与测试的模型名。
    pub model: String,
    /// 参与测试的服务地址。
    pub base_url: String,
}

/// 服务监听配置更新段。
#[derive(Clone, Deserialize)]
pub struct ServerConfigUpdate {
    /// 新监听地址；`None` 表示不修改。
    pub host: Option<String>,
    /// 新监听端口；`None` 表示不修改。
    pub port: Option<u16>,
    /// 新 CORS 来源列表；`None` 表示不修改。
    pub cors_origins: Option<Vec<String>>,
}

/// ouyi 8.0 上游配置更新段。
#[derive(Clone, Deserialize)]
pub struct OuyiConfigUpdate {
    /// 上游服务地址；`None` 表示不修改。
    pub api_base: Option<String>,
    /// 客户端版本标识；`None` 表示不修改。
    pub client_version: Option<String>,
    /// 前端传来的 jwt_token 字段（对应后端 ouyi.jwt_token）；脱敏占位值会被忽略。
    pub jwt_token: Option<String>,
    /// 模型列表刷新间隔（秒）；`None` 表示不修改。
    pub model_refresh_interval_secs: Option<u64>,
}

/// 平台配置更新段。
#[derive(Clone, Deserialize)]
pub struct PlatformConfigUpdate {
    /// 启动默认平台标识；`None` 表示不修改。
    pub default: Option<String>,
    /// 启动时是否提示确认；`None` 表示不修改。
    pub prompt_on_start: Option<bool>,
}

/// ouyi 7.0 上游配置更新段，各字段 `None` 表示不修改。
#[derive(Clone, Deserialize)]
pub struct Ouyi7ConfigUpdate {
    /// 上游服务根地址。
    pub base_url: Option<String>,
    /// 聊天 API 路径前缀。
    pub chatapi_prefix: Option<String>,
    /// JWT 凭据；脱敏占位值会被忽略以保留原值。
    pub jwt_token: Option<String>,
    /// 默认模型名。
    pub default_model: Option<String>,
    /// 模型列表刷新间隔（秒）。
    pub model_refresh_interval_secs: Option<u64>,
    /// 最大历史消息条数。
    pub max_history_messages: Option<usize>,
    /// 模拟流式输出的每个分块字符数。
    pub simulated_stream_chunk_chars: Option<usize>,
}

/// 代理配置更新段。
#[derive(Clone, Deserialize)]
pub struct ProxyConfigUpdate {
    /// 出站代理地址；`None` 表示不修改。
    pub url: Option<String>,
}

/// 管理员密码更新段：需同时提供旧密码与新密码才会生效。
#[derive(Clone, Deserialize)]
pub struct AdminUpdate {
    /// 当前密码，用于校验操作者身份。
    pub old_password: Option<String>,
    /// 要设置的新密码，不能为空。
    pub new_password: Option<String>,
}

/// 账号条目的原始请求形态（密码为明文，由服务端负责哈希）。
#[derive(Clone, Deserialize)]
pub struct AccountEntryRaw {
    /// 账号登录邮箱，作为唯一主键。
    pub email: String,
    /// 账号绑定手机号。
    pub mobile: String,
    /// 手机号区号；缺省为空串。
    #[serde(default)]
    pub area_code: String,
    /// 明文密码；为空时保留该账号已存的密码哈希。
    #[serde(default)]
    pub password: String,
    /// JWT 凭据；脱敏占位值或空串会保留/清空已存值。
    #[serde(default)]
    pub token: Option<String>,
    /// 是否启用；缺省为 `false`。
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
/// 串行化所有管理后台配置写操作的互斥锁，避免并发写配置文件产生竞态。
pub(super) static CONFIG_UPDATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 测试辅助：并发上限配置更新的失败类别。
#[derive(Debug)]
#[cfg(test)]
pub(super) enum LimitsConfigUpdateError {
    /// 数值超出允许范围（0..=1000）。
    Invalid(String),
    /// 配置文件持久化失败。
    Persistence(AppError),
}

/// 测试辅助：校验并持久化并发上限配置，成功后同步内存配置与限流器；
/// 持久化失败时不改动任何运行时状态。
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

/// 配置更新主流程完成后交付给调用方的剩余处理信息。
pub(super) struct PreparedConfigUpdate {
    /// 是否修改了服务监听地址/端口（需重启才能生效，调用方据此附提示）。
    server_changed: bool,
    /// 待整体替换的账号列表；`None` 表示本次更新未涉及账号。
    accounts: Option<Vec<AccountEntryRaw>>,
}

/// 配置更新流程的失败类别。
#[derive(Debug)]
pub(super) enum ConfigUpdateError {
    /// 请求参数非法（映射为 400）。
    BadRequest(String),
    /// 鉴权失败，如旧密码不正确、新密码为空（映射为 401）。
    Unauthorized(String),
    /// 配置写盘失败（映射为 500）。
    Persistence(AppError),
}

/// 校验合并配置更新，持久化到配置文件并提交运行时状态。
///
/// 副作用：写配置文件；按更新内容刷新内存配置、并发限流器、管理员密码
/// 哈希；当解析出的目标平台与当前活动平台不同时，重建上游客户端、切换
/// 平台标记、刷新平台状态，并后台异步重载模型列表。写盘失败时不提交
/// 任何内存改动。
///
/// 成功返回 [`PreparedConfigUpdate`] 供调用方继续处理账号替换；
/// 失败返回 [`ConfigUpdateError`]。
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
    // 运行时平台切换意图：用户在网页切换"当前平台"时，目标平台。
    // 仅当解析后的目标平台与当前活动平台不同时才会触发重建。
    let mut platform_switch_target: Option<crate::platform::OuyiPlatform> = None;

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
        // 网页"切换平台"语义：只要 default 与当前活动平台不同，就触发运行时切换。
        // active_platform 在副作用段读取，这里只记录意图，等写盘成功后才真正切换。
        let active_now = *state.platform.read().await;
        if platform.default != active_now {
            platform_switch_target = Some(platform.default);
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

    if let Some(api_keys_update) = update.api_keys.as_ref()
        && let Some(raw_keys) = api_keys_update.keys.as_ref()
    {
        let keys = crate::api_auth::normalize_keys(raw_keys)
            .map_err(ConfigUpdateError::BadRequest)?;
        // 空数组表示显式关闭鉴权并清空 key；数组非空但 normalize 后为空，说明
        // 全部为空白或掩码占位项，未提供真实 key，保留既有 api_keys 不变。
        if !keys.is_empty() || raw_keys.is_empty() {
            candidate.api_keys = Some(ApiKeysConfig { keys });
        }
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

    // 运行时平台切换（网页"切换平台"按钮）：用 persisted config 全量重建目标平台的
    // client —— 这覆盖了"同平台改 token"的局部重建路径，因为新 client 已经包含
    // 所有配置改动。仅当用户在 platform.default 字段实际改了选择时才走这条路径。
    if let Some(target) = platform_switch_target {
        // 用最新配置构造目标平台的 client
        let new_client =
            crate::build_ouyi_client(&state.http_client, &persisted, target);
        let new_base_url = match &new_client {
            crate::client::OuyiClient::V7(v7) => v7.base_url().to_string(),
            crate::client::OuyiClient::V8(_) => persisted.ouyi.base_url.clone(),
        };
        // 切换：先写 client，再切换 platform 标记，最后刷 platform_status
        *state.ouyi_client.write().await = new_client;
        *state.platform.write().await = target;
        {
            let mut status = state.platform_status.write().await;
            status.platform = target;
            status.label = target.label().to_string();
            status.base_url = new_base_url.clone();
            status.last_selected_at = chrono::Utc::now();
            // 切换后 token 是否有效要由后续 catalog 加载判定；先标记为未知
            status.token_valid = false;
            status.model_count = 0;
        }
        tracing::info!(target = target.label(), "已运行时切换平台");

        // 后台触发一次模型列表重载（不阻塞响应）。失败也不影响切换本身，
        // 后台刷新任务会在下个 tick 兜底。
        let reload_state = Arc::clone(state);
        tokio::spawn(async move {
            let client = reload_state.ouyi_client.read().await;
            match crate::load_platform_catalog(&client).await {
                Ok(catalog) => {
                    let count = catalog.models.len();
                    // V7：把路由表同步给 client（customAIModels/aiModels 的路由决策）
                    if let crate::client::OuyiClient::V7(v7) = &*client {
                        let table = crate::client::ouyi_v7::build_routing_table_from_catalog(&catalog);
                        v7.update_routing(table);
                    }
                    reload_state.model_mapper.load_from_catalog(&catalog);
                    let mut status = reload_state.platform_status.write().await;
                    status.model_count = reload_state.model_mapper.model_count();
                    status.token_valid = count > 0;
                    tracing::info!(platform = status.platform.label(), count, "切换平台后模型列表已刷新");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "切换平台后模型列表加载失败，等待后台刷新任务兜底");
                }
            }
        });
    } else {
        // 未切换平台：仅当当前活动平台的 token/base_url 变了才重建同平台 client
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
    }

    Ok(PreparedConfigUpdate {
        server_changed,
        accounts: update.accounts,
    })
}

/// 全量配置更新接口（`PUT /admin/api/config`）。
///
/// 解析请求体为 [`ConfigUpdate`]，在 [`CONFIG_UPDATE_LOCK`] 保护下执行
/// [`prepare_and_commit_config_update`]，随后按需整体替换账号池。
/// 副作用：写配置文件、更新内存配置 / 限流器 / 上游客户端 / 账号池。
/// 失败时返回：400（参数非法）、401（鉴权失败）、500（写盘失败）。
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

/// `GET /admin/api/logs` 响应元素：单条请求日志。
#[derive(Serialize)]
pub struct RequestLogResponse {
    /// 完成时间（Unix 秒时间戳）。
    timestamp: i64,
    /// 请求标识（按列表序号生成）。
    request_id: String,
    /// 请求使用的模型名。
    model: String,
    /// API key 标识（当前恒为 "single"）。
    api_key: String,
    /// 消耗的 prompt token 数。
    prompt_tokens: u64,
    /// 产出的 completion token 数。
    completion_tokens: u64,
    /// 请求耗时（毫秒）。
    latency_ms: u64,
    /// 是否成功。
    success: bool,
    /// 处理该请求的平台标识。
    platform: String,
}

/// `GET /admin/api/logs` 查询参数。
#[derive(Deserialize)]
pub struct LogsQuery {
    /// 返回条数上限；缺省为 100。
    limit: Option<usize>,
}

/// 查询最近的请求日志，按时间倒序返回。只读，无副作用。
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

/// 运行日志响应元素：单条运行日志。
#[derive(Serialize)]
pub struct RuntimeLogEntry {
    /// 产生时间（UTC，ISO8601 带毫秒）。
    timestamp: String,
    /// 日志级别（如 "INFO" / "WARN"）。
    level: String,
    /// 日志来源标识（当前恒为 "ouyi-api"）。
    target: String,
    /// 日志正文。
    message: String,
}

/// `GET /admin/api/runtime-logs` 响应体。
#[derive(Serialize)]
pub struct RuntimeLogsResponse {
    /// 命中的日志总数（分页前的全量条数）。
    total: usize,
    /// 本次返回的起始偏移。
    offset: usize,
    /// 本次返回的条数上限。
    limit: usize,
    /// 日志条目列表。
    logs: Vec<RuntimeLogEntry>,
}

/// `GET /admin/api/runtime-logs` 查询参数。
#[derive(Deserialize)]
pub struct RuntimeLogsQuery {
    /// 起始偏移；缺省为 0。
    offset: Option<usize>,
    /// 返回条数上限；缺省为 100。
    limit: Option<usize>,
}

/// 按偏移与条数分页查询运行日志。只读，无副作用。
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

/// `GET /admin/api/models/categorized` 响应体：按类别分组的模型列表。
#[derive(Serialize)]
pub struct CategorizedModelsResponse {
    /// 当前活动平台标识。
    platform: String,
    /// 按用途分类的模型分组。
    categories: Vec<ModelCategory>,
}

/// 返回按类别分组的当前平台模型列表。只读，无副作用。
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

/// `GET /admin/api/tool-runtime-logs` 响应元素：单条工具运行时日志。
#[derive(Serialize)]
pub struct ToolRuntimeLogResponse {
    /// 记录时间（UTC，ISO8601 带毫秒）。
    timestamp: String,
    /// 所属请求 ID。
    request_id: String,
    /// 请求入口端点。
    endpoint: String,
    /// 运行模式（"compiler" / "broker"）。
    mode: String,
    /// 工具模型提供方标识。
    provider: String,
    /// 工具模型名。
    model: String,
    /// 工具模型服务地址（已脱敏）。
    base_url: String,
    /// 决策结果（"tool_calls" / "final_answer"）。
    decision: String,
    /// 本次涉及的工具名列表。
    tool_names: Vec<String>,
    /// 消耗的 prompt token 数。
    prompt_tokens: u64,
    /// 产出的 completion token 数。
    completion_tokens: u64,
    /// 耗时（毫秒）。
    latency_ms: u64,
    /// 调用是否成功。
    success: bool,
    /// 失败时的错误描述；成功时为 `None`。
    error: Option<String>,
}

/// `GET /admin/api/tool-runtime-logs` 查询参数。
#[derive(Deserialize)]
pub struct ToolRuntimeLogsQuery {
    /// 返回条数上限；缺省为 100。
    limit: Option<usize>,
}

/// 查询最近的工具运行时日志，按时间倒序返回。只读，无副作用。
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

/// 工具模型连通性测试接口（anthropic 工具运行时）。
///
/// 用请求体（缺省字段取当前配置）构造测试配置，向工具模型发送一次最小
/// 请求（"ping"，max_tokens=8）。副作用：向上游发起 HTTP 请求，不落盘。
/// 失败时返回：400（未启用 / 配置缺失）、502（上游请求失败）。
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

/// 工具模型连通性测试接口（OpenAI 工具运行时）。
///
/// 行为与 [`admin_tool_runtime_test`] 相同，仅固定走 openai-compatible
/// 提供方。副作用：向上游发起 HTTP 请求，不落盘。
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

/// `GET /admin/api/accounts` 响应元素：账号的列表视图。
#[derive(Serialize)]
pub struct AccountView {
    /// 账号登录邮箱。
    pub email: String,
    /// 绑定手机号。
    pub mobile: String,
    /// 手机号区号。
    pub area_code: String,
    /// JWT 凭据（已脱敏）；未持有时为 `None`。
    pub token: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 运行状态（"idle" / "busy" / "error:<次数>" / "invalid"，
    /// 无运行时记录时为 "unknown"）。
    pub state: String,
    /// 累计被调度使用次数。
    pub request_count: u64,
}

/// 列出全部账号及其运行时状态。只读，无副作用；token 脱敏返回。
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

/// `POST /admin/api/accounts` 请求体：新增单个账号。
#[derive(Deserialize)]
pub struct AddAccountRequest {
    /// 账号登录邮箱（池内唯一）。
    pub email: String,
    /// 绑定手机号。
    pub mobile: String,
    /// 手机号区号；缺省为空串。
    #[serde(default)]
    pub area_code: String,
    /// 明文密码；为空则不设置，非空时由服务端 bcrypt 哈希后存储。
    #[serde(default)]
    pub password: String,
    /// JWT 凭据；空串视为未持有。
    pub token: Option<String>,
    /// 是否启用；缺省为 `false`。
    #[serde(default)]
    pub enabled: bool,
}

/// 新增单个账号；邮箱重复时返回 409，成功返回 200。
/// 副作用：修改内存账号池。
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

/// `POST /admin/api/accounts/remove` 请求体。
#[derive(Deserialize)]
pub struct RemoveAccountRequest {
    /// 要移除的账号邮箱。
    pub email: String,
}

/// 移除指定邮箱的账号；不存在时返回 404，成功返回 200。
/// 副作用：整体替换内存账号池内容。
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

/// 返回账号池中所有已持有 token 的脱敏列表及数量；仅读取内存数据，
/// 不向上游请求任何内容。
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
