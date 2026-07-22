pub(crate) mod accounts;
pub(crate) mod auth;
mod handlers;
pub(crate) mod stats;

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

fn extract_bearer_token(req: &Request) -> Option<&str> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

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

#[derive(rust_embed::Embed)]
#[folder = "web/dist/"]
struct WebAssets;

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
}
