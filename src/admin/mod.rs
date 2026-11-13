//! 管理后台模块：JWT 鉴权中间件、后台 API 路由与前端静态资源服务。
//!
//! - [`accounts`]: 上游账号池存储与运行时状态
//! - [`auth`]: 密码哈希校验、JWT 签发验证与登录限流
//! - [`handlers`]: 各后台 API 端点处理器
//! - [`stats`]: 请求 / 运行时 / 工具运行时统计采集

/// 上游账号池存储与运行时状态。
pub(crate) mod accounts;
/// 管理员密码哈希、JWT 签发验证与登录限流。
pub(crate) mod auth;
/// 后台 API 端点处理器（仅本模块内挂载路由使用）。
mod handlers;
/// 请求与运行日志的统计采集。
pub(crate) mod stats;

/// 账号池存储的 crate 内统一导出。
pub(crate) use accounts::AccountStore;

use std::sync::Arc;

use axum::extract::Request;
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};

use crate::AppState;

/// 组装管理后台 API 路由，并挂载 JWT 鉴权中间件
/// （`/admin/api/setup` 与 `/admin/api/login` 豁免校验）。
pub(crate) fn admin_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin/api/setup", post(handlers::admin_setup))
        .route("/admin/api/login", post(handlers::admin_login))
        .route("/admin/api/status", get(handlers::admin_status))
        .route("/admin/api/stats", get(handlers::admin_stats))
        .route("/admin/api/models", get(handlers::admin_models))
        .route(
            "/admin/api/models/categorized",
            get(handlers::admin_categorized_models),
        )
        .route("/admin/api/config", get(handlers::admin_config))
        .route("/admin/api/config", put(handlers::admin_put_config))
        .route("/admin/api/logs", get(handlers::admin_logs))
        .route("/admin/api/runtime-logs", get(handlers::admin_runtime_logs))
        .route(
            "/admin/api/tool-runtime-logs",
            get(handlers::admin_tool_runtime_logs),
        )
        .route(
            "/admin/api/tool-runtime/test",
            post(handlers::admin_tool_runtime_test),
        )
        .route(
            "/admin/api/openai-tool-runtime/test",
            post(handlers::admin_openai_tool_runtime_test),
        )
        .route("/admin/api/accounts", get(handlers::admin_list_accounts))
        .route("/admin/api/accounts", post(handlers::admin_add_account))
        .route(
            "/admin/api/accounts/remove",
            post(handlers::admin_remove_account),
        )
        .route(
            "/admin/api/accounts/refresh",
            get(handlers::admin_refresh_token),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            jwt_middleware,
        ))
        .with_state(state)
}

/// 鉴权中间件：校验请求 `Authorization: Bearer <JWT>`，无效时返回 401；
/// setup 与 login 两个路径跳过校验直接放行。
async fn jwt_middleware(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let skip_paths = ["/admin/api/setup", "/admin/api/login"];
    let path = req.uri().path();
    if skip_paths.contains(&path) {
        return next.run(req).await;
    }

    let token = extract_bearer_token(&req);
    let valid = match token {
        Some(t) => {
            let store = state.admin_store.read().await;
            auth::verify_jwt(&store, t)
        }
        None => false,
    };

    if !valid {
        tracing::debug!(path, "401 unauthorized admin request");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    next.run(req).await
}

/// 从请求头提取 Bearer token 字符串；缺失或格式不符时返回 `None`。
fn extract_bearer_token(req: &Request) -> Option<&str> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

/// 组装前端静态资源路由：`web/dist` 目录存在时以目录服务挂载 `/admin`
/// （SPA 回退到 index.html）；目录不存在时回退到编译期内嵌资源服务。
pub(crate) fn static_file_router() -> Router<Arc<AppState>> {
    use tower_http::services::{ServeDir, ServeFile};

    let web_dist = std::path::Path::new("web/dist");
    if web_dist.exists() {
        Router::new().nest_service(
            "/admin",
            ServeDir::new(web_dist).fallback(ServeFile::new("web/dist/index.html")),
        )
    } else {
        Router::new().fallback(serve_embedded_fallback)
    }
}

/// 编译期内嵌的前端静态资源（用于 `web/dist` 目录不存在时的回退服务）。
#[derive(rust_embed::Embed)]
#[folder = "web/dist/"]
struct WebAssets;

/// 从内嵌资源服务 `/admin/**` 路径：命中静态文件按 MIME 返回，未命中或
/// 目录请求回退 `index.html`（SPA 路由）；非 admin 路径或无内嵌资源返回 404。
async fn serve_embedded_fallback(uri: axum::http::Uri) -> Response {
    let path = uri.path();
    if path == "/admin" || path.starts_with("/admin/") {
        let key = path
            .strip_prefix("/admin/")
            .unwrap_or("")
            .trim_start_matches('/');
        if !key.is_empty() {
            if let Some(content) = WebAssets::get(key) {
                let mime = mime_guess::from_path(key).first_or_octet_stream();
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime.as_ref())],
                    content.data,
                )
                    .into_response();
            }
        }
        if let Some(content) = WebAssets::get("index.html") {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                content.data,
            )
                .into_response();
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use crate::admin::auth::AdminStore;
    use crate::admin::stats::StatsCollector;
    use crate::client::http::OuyiHttpClient;
    use crate::config::{
        AdminConfig, AppConfig, LimitsConfig, OuyiConfig, PlatformConfig, ServerConfig,
        ToolCallConfig,
    };
    use crate::model::mapper::ModelMapper;
    use crate::platform::{OuyiPlatform, PlatformStatus};

    struct TempFileGuard(PathBuf);

    impl TempFileGuard {
        fn new(label: &str) -> Self {
            Self(std::env::temp_dir().join(format!(
                "ouyi-admin-limits-{label}-{}.toml",
                uuid::Uuid::new_v4()
            )))
        }
    }

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn write_complete_test_config(path: &std::path::Path, concurrency: usize) {
        std::fs::write(
            path,
            format!(
                r#"# preserved
[server]
host = "127.0.0.1"
port = 0

[ouyi]
base_url = "http://127.0.0.1:1"
jwt_token = "test-jwt"
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 5
no_data_timeout_secs = 5
max_response_bytes = 1048576
max_messages_bytes = 1048576
max_concurrent_requests = {concurrency}

[admin]
jwt_secret = "test-secret"

[platform]
default = "ouyi8"
prompt_on_start = false
"#
            ),
        )
        .unwrap();
    }

    fn make_test_state() -> Arc<AppState> {
        make_test_state_with_limit(0)
    }

    fn make_test_state_with_limit(max_concurrent_requests: usize) -> Arc<AppState> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();
        let base_url = "http://127.0.0.1:1".to_string();
        let platform = OuyiPlatform::Ouyi8;
        let config = AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
            },
            ouyi: OuyiConfig {
                base_url: base_url.clone(),
                jwt_token: "test-jwt".to_string(),
                app_version: "3.0.3".to_string(),
                model_refresh_interval_secs: 3600,
            },
            limits: LimitsConfig {
                request_timeout_secs: 5,
                no_data_timeout_secs: 5,
                max_response_bytes: 1024 * 1024,
                max_messages_bytes: 1024 * 1024,
                max_concurrent_requests,
            },
            admin: Some(AdminConfig {
                jwt_secret: Some("test-secret".to_string()),
                password_hash: None,
            }),
            tool_call: ToolCallConfig::default(),
            tool_runtime: None,
            openai_tool_runtime: None,
            platform: Some(PlatformConfig {
                default: platform,
                prompt_on_start: false,
            }),
            ouyi_v7: None,
        };

        Arc::new(AppState {
            concurrency_limiter: crate::concurrency::ConcurrencyLimiter::new(
                max_concurrent_requests,
            ),
            config: tokio::sync::RwLock::new(config),
            http_client: http_client.clone(),
            ouyi_client: tokio::sync::RwLock::new(crate::client::OuyiClient::V8(
                OuyiHttpClient::new(
                    http_client,
                    base_url.clone(),
                    "test-jwt".to_string(),
                    "3.0.3".to_string(),
                ),
            )),
            active_sessions: dashmap::DashMap::new(),
            tool_sessions: dashmap::DashMap::new(),
            tool_ledger: crate::tool_runtime::ToolLedger::new(),
            responses_state: crate::responses_state::ResponsesState::default(),
            model_mapper: mapper,
            admin_store: tokio::sync::RwLock::new(AdminStore::new(
                "test-secret".to_string(),
                String::new(),
            )),
            account_store: AccountStore::new(),
            stats: StatsCollector::new(),
            platform: tokio::sync::RwLock::new(platform),
            platform_status: tokio::sync::RwLock::new(PlatformStatus::new(platform, base_url)),
        })
    }

    #[tokio::test]
    async fn protected_admin_api_requires_bearer_token() {
        let state = make_test_state();
        let app = admin_routes(state.clone()).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{}/admin/api/status", addr))
            .send()
            .await
            .unwrap();
        server.abort();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn spawn_admin_server(state: Arc<AppState>) -> (String, tokio::task::JoinHandle<()>) {
        let app = admin_routes(state.clone()).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), server)
    }

    async fn admin_token(state: &AppState) -> String {
        state
            .admin_store
            .read()
            .await
            .generate_token("admin")
            .unwrap()
    }

    #[tokio::test]
    async fn authenticated_config_get_returns_in_memory_concurrency_limit() {
        let state = make_test_state_with_limit(7);
        let token = admin_token(&state).await;
        let (base_url, server) = spawn_admin_server(state).await;

        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{base_url}/admin/api/config"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        server.abort();

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["limits"]["max_concurrent_requests"], 7);
    }

    #[tokio::test]
    async fn config_put_rejects_invalid_concurrency_limits_without_mutating_runtime() {
        let state = make_test_state_with_limit(7);
        let token = admin_token(&state).await;
        let (base_url, server) = spawn_admin_server(state.clone()).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        for invalid in [json!(-1), json!(1.5), json!(1001)] {
            let response = client
                .put(format!("{base_url}/admin/api/config"))
                .bearer_auth(&token)
                .json(&json!({
                    "limits": {
                        "max_concurrent_requests": invalid
                    }
                }))
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["ok"], false);
            assert!(body["error"].is_string());
            assert_eq!(state.config.read().await.limits.max_concurrent_requests, 7);
            assert_eq!(state.concurrency_limiter.limit(), 7);
        }

        server.abort();
    }

    #[tokio::test]
    async fn limits_update_helper_persists_then_updates_memory_and_limiter() {
        let state = make_test_state_with_limit(9);
        let config_file = TempFileGuard::new("success");
        std::fs::write(
            &config_file.0,
            "[limits]\nmax_concurrent_requests = 9\n\n[server]\nport = 3000\n",
        )
        .unwrap();

        handlers::apply_limits_config_update(&state, &config_file.0, 3)
            .await
            .unwrap();

        let persisted: toml::Value = std::fs::read_to_string(&config_file.0)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            persisted["limits"]["max_concurrent_requests"].as_integer(),
            Some(3)
        );
        assert_eq!(state.config.read().await.limits.max_concurrent_requests, 3);
        assert_eq!(state.concurrency_limiter.limit(), 3);
    }

    #[tokio::test]
    async fn limits_update_helper_keeps_runtime_unchanged_when_persistence_fails() {
        for (label, contents) in [("missing", None), ("invalid", Some("not = [valid"))] {
            let state = make_test_state_with_limit(9);
            let config_file = TempFileGuard::new(label);
            if let Some(contents) = contents {
                std::fs::write(&config_file.0, contents).unwrap();
            }

            let result = handlers::apply_limits_config_update(&state, &config_file.0, 3).await;

            assert!(result.is_err(), "{label}");
            assert_eq!(
                state.config.read().await.limits.max_concurrent_requests,
                9,
                "{label}"
            );
            assert_eq!(state.concurrency_limiter.limit(), 9, "{label}");
        }
    }

    #[tokio::test]
    async fn full_config_update_rejects_bad_password_without_partial_concurrency_commit() {
        let state = make_test_state_with_limit(1);
        state
            .admin_store
            .write()
            .await
            .set_password_hash(AdminStore::hash_password("correct-password").unwrap());
        let config_file = TempFileGuard::new("bad-password-transaction");
        write_complete_test_config(&config_file.0, 1);
        let before = std::fs::read(&config_file.0).unwrap();
        let update = serde_json::from_value(json!({
            "limits": {"max_concurrent_requests": 2},
            "admin": {
                "old_password": "wrong-password",
                "new_password": "replacement-password"
            }
        }))
        .unwrap();

        let result =
            handlers::prepare_and_commit_config_update(&state, &config_file.0, update).await;

        assert!(matches!(
            result,
            Err(handlers::ConfigUpdateError::Unauthorized(_))
        ));
        assert_eq!(std::fs::read(&config_file.0).unwrap(), before);
        assert_eq!(state.config.read().await.limits.max_concurrent_requests, 1);
        assert_eq!(state.concurrency_limiter.limit(), 1);
    }

    #[tokio::test]
    async fn full_config_update_persists_two_and_reloads_it_after_restart() {
        let state = make_test_state_with_limit(1);
        let config_file = TempFileGuard::new("reload-two");
        write_complete_test_config(&config_file.0, 1);
        let update = serde_json::from_value(json!({
            "limits": {"max_concurrent_requests": 2}
        }))
        .unwrap();

        handlers::prepare_and_commit_config_update(&state, &config_file.0, update)
            .await
            .unwrap();

        let reloaded = AppConfig::load_from_path(&config_file.0).unwrap();
        assert_eq!(reloaded.limits.max_concurrent_requests, 2);
        assert_eq!(state.config.read().await.limits.max_concurrent_requests, 2);
        assert_eq!(state.concurrency_limiter.limit(), 2);
    }

    #[tokio::test]
    async fn config_update_lock_serializes_admin_put_mutations() {
        let first_guard = handlers::CONFIG_UPDATE_LOCK.lock().await;
        let mut second_task = tokio::spawn(async {
            let _second_guard = handlers::CONFIG_UPDATE_LOCK.lock().await;
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second_task)
                .await
                .is_err()
        );

        drop(first_guard);
        tokio::time::timeout(std::time::Duration::from_secs(1), second_task)
            .await
            .unwrap()
            .unwrap();
    }

    /// 网页"切换平台"：把 platform.default 从 Ouyi8 改为 Ouyi7 时，
    /// 写盘成功后应立即更新 state.platform、state.ouyi_client（变体切换为 V7）、
    /// state.platform_status。这是无需重启的核心保证。
    #[tokio::test]
    async fn platform_switch_updates_runtime_platform_client_and_status() {
        let state = make_test_state_with_limit(0);
        // make_test_state 默认 platform = Ouyi8、ouyi_v7 = None
        assert_eq!(*state.platform.read().await, OuyiPlatform::Ouyi8);
        let config_file = TempFileGuard::new("platform-switch");
        write_complete_test_config(&config_file.0, 0);
        let update = serde_json::from_value(json!({
            "platform": {"default": "ouyi7", "prompt_on_start": false}
        }))
        .unwrap();

        handlers::prepare_and_commit_config_update(&state, &config_file.0, update)
            .await
            .unwrap();

        // state.platform 已切换
        assert_eq!(*state.platform.read().await, OuyiPlatform::Ouyi7);
        // state.ouyi_client 已重建为 V7 变体
        assert!(matches!(
            *state.ouyi_client.read().await,
            crate::client::OuyiClient::V7(_)
        ));
        // platform_status 同步
        let status = state.platform_status.read().await;
        assert_eq!(status.platform, OuyiPlatform::Ouyi7);
        assert_eq!(status.label, "Ouyi 7.0");
        // 持久化的 config 也带上新 default
        assert_eq!(
            state.config.read().await.platform.as_ref().unwrap().default,
            OuyiPlatform::Ouyi7
        );
    }

    /// 同平台改 token：用户没切换平台时（platform.default 与活动平台一致），
    /// 走"同平台 client 重建"分支。
    #[tokio::test]
    async fn platform_default_unchanged_keeps_legacy_same_platform_rebuild() {
        let state = make_test_state_with_limit(0);
        // 活动 platform 是 Ouyi8
        let config_file = TempFileGuard::new("platform-same");
        write_complete_test_config(&config_file.0, 0);
        let update = serde_json::from_value(json!({
            "deepseek": {"jwt_token": "refreshed-ouyi8-jwt"}
        }))
        .unwrap();

        handlers::prepare_and_commit_config_update(&state, &config_file.0, update)
            .await
            .unwrap();

        // 平台没变
        assert_eq!(*state.platform.read().await, OuyiPlatform::Ouyi8);
        // client 仍是 V8（同平台重建，不切换变体）
        assert!(matches!(
            *state.ouyi_client.read().await,
            crate::client::OuyiClient::V8(_)
        ));
    }

    /// 切换到目标平台与当前活动平台相同时，不应触发重建（幂等）。
    #[tokio::test]
    async fn platform_switch_to_same_active_platform_is_noop() {
        let state = make_test_state_with_limit(0);
        let config_file = TempFileGuard::new("platform-noop");
        write_complete_test_config(&config_file.0, 0);
        let update = serde_json::from_value(json!({
            "platform": {"default": "ouyi8", "prompt_on_start": false}
        }))
        .unwrap();

        handlers::prepare_and_commit_config_update(&state, &config_file.0, update)
            .await
            .unwrap();

        assert_eq!(*state.platform.read().await, OuyiPlatform::Ouyi8);
        // 没有切换意图，client 不应被重建（这里只能间接验证——不 panic、不切换变体）
        assert!(matches!(
            *state.ouyi_client.read().await,
            crate::client::OuyiClient::V8(_)
        ));
    }

    /// 回归测试：当 config.toml 没有 [platform] 段（config.platform = None）
    /// 但服务已用默认平台启动时，GET /admin/api/config 必须返回非 null 的
    /// platform.default（与当前活动平台一致），否则前端的 persistedPlatformDefault=null
    /// 会导致"立即切换平台"按钮永远灰色（即使选了不同的平台）。
    #[tokio::test]
    async fn admin_config_returns_active_platform_when_config_section_missing() {
        let state = make_test_state_with_limit(0);
        // 模拟 yn8 生产现状：config.toml 无 [platform] 段，但 state.platform = Ouyi8
        state.config.write().await.platform = None;
        let token = admin_token(&state).await;
        let (base_url, server) = spawn_admin_server(state).await;

        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{base_url}/admin/api/config"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        server.abort();

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        // platform 不能是 null
        assert!(
            body["platform"].is_object(),
            "platform 应该是非 null 对象，实际: {}",
            body["platform"]
        );
        // default 必须等于当前活动平台 Ouyi8（即 "ouyi8"）
        assert_eq!(body["platform"]["default"], "ouyi8");
        // prompt_on_start 缺省为 true（与 PlatformConfig 的兜底一致）
        assert_eq!(body["platform"]["prompt_on_start"], true);
    }
}
