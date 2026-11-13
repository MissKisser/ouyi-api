//! 协议适配层。
//!
//! 负责对外 API 协议与上游 ouyi 模型之间的双向转换，按方向包含四个部分：
//! - 请求适配：`request`（OpenAI ChatCompletion → ouyi 文本协议）、
//!   `anthropic_request`（Anthropic Messages → ouyi 文本协议）、
//!   `ouyi_v7_request`（OpenAI / Anthropic → ouyi 7.0 结构化输入）。
//! - 响应适配：`response`（ouyi SSE 事件 → OpenAI 流式 / 非流式响应）、
//!   `anthropic_response`（ouyi 增量输出 → Anthropic 流式 / 非流式消息）。
//! - 工具调用：`tool_call`（模型文本中工具调用标记的流式解析与 JSON 自修复）、
//!   `openai_tool_response`（编译后的工具调用 → OpenAI 响应体与 SSE chunk 构造）。
//! - 降级输出：`ouyi_v7_response`（纯文本 → 各协议的非流式响应或模拟流式 SSE）。
/// Anthropic Messages 请求 → ouyi 文本协议的转换。
pub mod anthropic_request;
/// ouyi 增量输出 → Anthropic 流式 / 非流式消息的转换。
pub mod anthropic_response;
/// 编译后的工具调用 → OpenAI 响应体与 SSE chunk 的构造。
pub mod openai_tool_response;
/// OpenAI / Anthropic 请求 → ouyi 7.0 结构化输入的转换。
pub mod ouyi_v7_request;
/// ouyi 7.0 纯文本输出 → 各协议非流式响应或模拟流式 SSE 的降级构造。
pub mod ouyi_v7_response;
/// OpenAI ChatCompletion 请求 → ouyi 文本协议的转换。
pub mod request;
/// ouyi SSE 事件 → OpenAI 流式 / 非流式响应的转换。
pub mod response;
/// 模型文本中工具调用标记的流式解析与 JSON 自修复。
pub mod tool_call;
