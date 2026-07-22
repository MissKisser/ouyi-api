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

    use axum::http::StatusCode;

    use crate::admin::auth::AdminStore;
    use crate::admin::stats::StatsCollector;
    use crate::client::http::OuyiHttpClient;
    use crate::config::{
        AdminConfig, AppConfig, LimitsConfig, OuyiConfig, PlatformConfig, ServerConfig,
        ToolCallConfig,
    };
    use crate::model::mapper::ModelMapper;
    use crate::platform::{OuyiPlatform, PlatformStatus};

    fn make_test_state() -> Arc<AppState> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
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
                max_concurrent_requests: 0,
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
            concurrency_limiter: crate::concurrency::ConcurrencyLimiter::new(0),
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

        let response = reqwest::get(format!("http://{}/admin/api/status", addr))
            .await
            .unwrap();
        server.abort();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
