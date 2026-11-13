//! 应用统一错误类型模块。
//!
//! [`AppError`] 汇总配置、上游请求、工具协议、SSE 流等场景的错误，并实现
//! `IntoResponse` 把每种变体映射为固定的 HTTP 状态码与 OpenAI 风格错误 JSON，
//! 供各处理器用 `?` 直接向上传播错误。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// 应用统一错误类型，各变体对应一类故障场景。
/// 经 `IntoResponse` 转为 HTTP 响应时，每个变体映射到固定的状态码与 `error.type` 字符串。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 配置缺失、非法或配置文件读写失败；映射为 500 `config_error`。
    #[error("配置错误: {0}")]
    Config(String),

    /// 上游平台请求失败（网络错误、非预期响应等）；映射为 502 `upstream_error`。
    #[error("上游请求失败: {0}")]
    Upstream(String),

    /// 工具协议编译失败（无法把工具定义编译为上游可识别的形式）；映射为 502 `tool_protocol_compile_failed`。
    #[error("工具协议编译失败: {0}")]
    ToolProtocol(String),

    /// 工具运行时的内部请求回流到推理入口，被递归防护拦截；映射为 502 `tool_runtime_recursion`。
    #[error("工具运行时递归调用: {0}")]
    ToolRecursion(String),

    /// SSE 流式传输中断或格式异常；映射为 502 `stream_error`。
    #[error("SSE流异常: {0}")]
    SseStream(String),

    /// 请求的模型在模型目录中不存在；映射为 404 `model_not_found`。
    #[error("模型未找到: {0}")]
    ModelNotFound(String),

    /// Responses 协议按 ID 续轮时找不到对应会话（不存在或已过期）；映射为 404 `response_not_found`。
    #[error("Responses 续轮不存在或已过期: {0}")]
    ResponseNotFound(String),

    /// 请求体超过大小限制；映射为 413 `payload_too_large`。
    #[error("请求过大: {0}")]
    PayloadTooLarge(String),

    /// 请求参数非法；映射为 400 `bad_request`。
    #[error("无效请求: {0}")]
    BadRequest(String),

    /// 管理后台认证失败（JWT 未配置或已过期）；映射为 401 `authentication_error`。
    #[error("认证失败: JWT token未配置或已过期")]
    Unauthorized,

    /// 未归类的内部错误；映射为 500 `internal_error`。
    #[error("内部错误: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    /// 把错误转换为 HTTP 响应：按变体选择状态码与 `error.type`，
    /// 响应体为 `{ "error": { "message", "type", "code" } }` 的 JSON，`code` 为状态码数值。
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
    /// 把 reqwest 网络错误包装为 [`AppError::Upstream`]，便于处理器用 `?` 传播上游故障。
    fn from(e: reqwest::Error) -> Self {
        AppError::Upstream(e.to_string())
    }
}
