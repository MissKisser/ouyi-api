use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("上游请求失败: {0}")]
    Upstream(String),

    #[error("工具协议编译失败: {0}")]
    ToolProtocol(String),

    #[error("工具运行时递归调用: {0}")]
    ToolRecursion(String),

    #[error("SSE流异常: {0}")]
    SseStream(String),

    #[error("模型未找到: {0}")]
    ModelNotFound(String),

    #[error("Responses 续轮不存在或已过期: {0}")]
    ResponseNotFound(String),

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
            AppError::ToolProtocol(_) => (StatusCode::BAD_GATEWAY, "tool_protocol_compile_failed"),
            AppError::ToolRecursion(_) => (StatusCode::BAD_GATEWAY, "tool_runtime_recursion"),
            AppError::SseStream(_) => (StatusCode::BAD_GATEWAY, "stream_error"),
            AppError::ModelNotFound(_) => (StatusCode::NOT_FOUND, "model_not_found"),
            AppError::ResponseNotFound(_) => (StatusCode::NOT_FOUND, "response_not_found"),
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
