use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::handler;
use crate::AppState;

pub fn app() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/models", get(handler::models::list_models))
        .route("/v1/chat/completions", post(handler::chat::chat_completions))
        .route(
            "/openai/v1/models",
            get(handler::models::list_models),
        )
        .route(
            "/openai/v1/responses",
            post(handler::responses::responses),
        )
        .route(
            "/anthropic/v1/models",
            get(handler::anthropic::list_models),
        )
        .route(
            "/anthropic/v1/messages",
            post(handler::anthropic::messages),
        )
        .route(
            "/anthropic/messages",
            post(handler::anthropic::messages),
        )
}
