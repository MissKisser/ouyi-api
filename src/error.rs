use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("上游请求失败: {0}")]
    Upstream(String),

    #[error("SSE流异常: {0}")]
    SseStream(String),

    #[error("模型未找到: {0}")]
    ModelNotFound(String),

    #[error("请求过大: {0}")]
    PayloadTooLarge(String),

    #[error("无效请求: {0}")]
    BadRequest(String),

    #[error("认证失败: JWT token未配置或已过期")]
    Unauthorized,

    #[error("内部错误: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_type) = match &self {
            AppError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, "config_error"),
            AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_error"),
            AppError::SseStream(_) => (StatusCode::BAD_GATEWAY, "stream_error"),
            AppError::ModelNotFound(_) => (StatusCode::NOT_FOUND, "model_not_found"),
            AppError::PayloadTooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "authentication_error"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };

        let body = json!({
            "error": {
                "message": self.to_string(),
                "type": error_type,
                "code": status.as_u16(),
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::Upstream(e.to_string())
    }
}
