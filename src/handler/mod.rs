//! HTTP 推理入口层：把 axum 路由挂载到各协议处理器上。
//! 各子模块分别负责一种对外协议端点：
//! - `anthropic`：Anthropic Messages API（`POST /anthropic/v1/messages`、`POST /anthropic/messages`）
//!   与 Anthropic 风格模型列表（`GET /anthropic/v1/models`）。
//! - `chat`：OpenAI Chat Completions API（`POST /v1/chat/completions`、`POST /openai/v1/chat/completions`）。
//! - `models`：OpenAI 风格模型列表（`GET /v1/models`、`GET /openai/v1/models`）。
//! - `responses`：OpenAI Responses API（`POST /v1/responses`、`POST /openai/v1/responses`），
//!   含流式（SSE）与非流式两种响应形态。

/// Anthropic Messages API 与 Anthropic 风格模型列表处理器。
pub mod anthropic;
/// OpenAI Chat Completions API 处理器。
pub mod chat;
/// OpenAI 风格模型列表处理器。
pub mod models;
/// OpenAI Responses API 处理器，含流式与非流式两种响应形态。
pub mod responses;
