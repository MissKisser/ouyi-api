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
