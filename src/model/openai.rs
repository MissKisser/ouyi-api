//! OpenAI 协议的数据模型定义。
//! 覆盖 Chat Completions（请求 / 流式 chunk / 非流式响应 / 工具调用）、
//! `/v1/models` 列表响应，以及 Responses API（请求 / 输入项 / 响应体），
//! 供协议接入层与欧易侧的协议转换使用。

use serde::{Deserialize, Serialize};

/// OpenAI Chat Completions 接口（`/v1/chat/completions`）的请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    /// 目标模型标识。
    pub model: String,
    /// 对话历史消息列表。
    pub messages: Vec<ChatMessage>,
    /// 是否以 SSE 流式返回；缺省时按非流式处理。
    #[serde(default)]
    pub stream: bool,
    /// 采样温度；当前流程未消费，保留以兼容客户端字段。
    #[serde(default)]
    #[allow(dead_code)]
    pub temperature: Option<f64>,
    /// 最大生成 token 数；当前流程未消费，保留以兼容客户端字段。
    #[serde(default)]
    #[allow(dead_code)]
    pub max_tokens: Option<u64>,
    /// 本次请求可调用的工具定义列表；缺省时表示不提供工具。
    #[serde(default)]
    pub tools: Option<Vec<ToolDef>>,
    /// 工具选择策略，透传的结构由上游协议约定；缺省时按 auto 处理。
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    /// 是否允许并行工具调用；当前流程未消费，保留以兼容客户端字段。
    #[serde(default)]
    #[allow(dead_code)]
    pub parallel_tool_calls: Option<bool>,
}

/// Chat 协议中的单条消息。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    /// 发送方角色，如 `"system"`、`"user"`、`"assistant"`、`"tool"`。
    pub role: String,
    /// 文本内容；工具调用消息等内容为空的场景下为 `None`。
    #[serde(default)]
    pub content: Option<String>,
    /// 助手消息携带的工具调用列表；非助手消息为 `None`。
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具结果消息对应的调用 ID；当前流程未消费，保留以兼容客户端字段。
    #[serde(default)]
    #[allow(dead_code)]
    pub tool_call_id: Option<String>,
    /// 消息名称标识；缺省时为 `None`。
    #[serde(default)]
    pub name: Option<String>,
}

/// OpenAI 工具定义，`type` 字段当前固定为 `"function"`。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolDef {
    /// 工具类型，序列化时映射为 `"type"`，当前取值为 `"function"`。
    #[serde(rename = "type")]
    pub tool_type: String,
    /// 函数定义本体。
    pub function: ToolFunction,
}

/// 工具的函数定义。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolFunction {
    /// 函数名，模型通过该名称发起调用。
    pub name: String,
    /// 函数功能描述；缺省时序列化会跳过该字段。
    #[serde(default)]
    pub description: Option<String>,
    /// 函数入参的 JSON Schema 定义；缺省时序列化会跳过该字段。
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

/// 一次工具调用记录。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    /// 调用唯一标识，工具结果需回填该 ID。
    pub id: String,
    /// 调用类型，序列化时映射为 `"type"`，当前取值为 `"function"`。
    #[serde(rename = "type")]
    pub call_type: String,
    /// 被调用函数的信息。
    pub function: ToolCallFunction,
}

/// 工具调用的函数信息。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallFunction {
    /// 被调用函数的名称。
    pub name: String,
    /// 函数入参，为 JSON 文本的字符串形式（流式场景可能不完整）。
    pub arguments: String,
}

/// Chat 流式响应的单个 SSE chunk。
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    /// 本次补全的唯一 ID，同一流内所有 chunk 一致。
    pub id: String,
    /// 对象类型常量，固定为 `"chat.completion.chunk"`。
    pub object: String,
    /// 创建时间戳（Unix 秒）。
    pub created: u64,
    /// 响应模型标识。
    pub model: String,
    /// 本 chunk 携带的选择项（通常只有一个）。
    pub choices: Vec<ChunkChoice>,
    /// 用量统计，仅在流结束时携带；缺省时序列化会跳过该字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// 流式 chunk 中的单个选择项。
#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    /// 选择项下标，通常为 0。
    pub index: u32,
    /// 本步增量内容。
    pub delta: ChunkDelta,
    /// 结束原因（如 `"stop"`、`"tool_calls"`）；流中途为 `None`，序列化时跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// 流式增量内容。
#[derive(Debug, Clone, Serialize)]
pub struct ChunkDelta {
    /// 首个 chunk 携带的角色（如 `"assistant"`）；其余为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// 本步新增的文本片段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 本步新增的工具调用片段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// 流式工具调用增量。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDelta {
    /// 工具调用在增量列表中的下标。
    pub index: u32,
    /// 调用唯一标识，仅首个片段携带。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// 调用类型，序列化时映射为 `"type"`，通常为 `"function"`。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    /// 函数名与参数片段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallFunctionDelta>,
}

/// 流式工具调用的函数片段。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallFunctionDelta {
    /// 被调用函数名称，仅首个片段携带。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 本步新增的入参 JSON 片段，客户端按序拼接后整体解析。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Chat Completions 非流式响应体。
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    /// 本次补全的唯一 ID。
    pub id: String,
    /// 对象类型常量，固定为 `"chat.completion"`。
    pub object: String,
    /// 创建时间戳（Unix 秒）。
    pub created: u64,
    /// 响应模型标识。
    pub model: String,
    /// 候选结果列表（当前流程只产生一项）。
    pub choices: Vec<ResponseChoice>,
    /// token 用量统计。
    pub usage: Usage,
}

/// 非流式响应中的单个选择项。
#[derive(Debug, Clone, Serialize)]
pub struct ResponseChoice {
    /// 选择项下标，通常为 0。
    pub index: u32,
    /// 助手输出消息。
    pub message: ResponseMessage,
    /// 结束原因，如 `"stop"`、`"tool_calls"`、`"length"`。
    pub finish_reason: String,
}

/// 非流式响应中的助手消息。
#[derive(Debug, Clone, Serialize)]
pub struct ResponseMessage {
    /// 角色，固定为 `"assistant"`。
    pub role: String,
    /// 文本内容；纯工具调用消息为 `None`，序列化时跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 工具调用列表；无调用时为 `None`，序列化时跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// token 用量统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// 输入消耗的 token 数。
    pub prompt_tokens: u64,
    /// 输出消耗的 token 数。
    pub completion_tokens: u64,
    /// 总 token 数（输入 + 输出）。
    pub total_tokens: u64,
}

/// `/v1/models` 接口的响应体。
#[derive(Debug, Clone, Serialize)]
pub struct ModelListResponse {
    /// 对象类型常量，固定为 `"list"`。
    pub object: String,
    /// 模型信息列表。
    pub data: Vec<ModelInfo>,
}

/// `/v1/models` 列表中的单个模型条目。
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    /// 模型标识。
    pub id: String,
    /// 对象类型常量，固定为 `"model"`。
    pub object: String,
    /// 创建时间戳（Unix 秒）。
    pub created: u64,
    /// 模型归属方（供应商标识）。
    pub owned_by: String,
}

/// OpenAI Responses API（`/v1/responses`）的请求体。
/// `input` 的结构较灵活，反序列化为原始 JSON 后再按项解析。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    /// 目标模型标识。
    pub model: String,
    /// 输入内容，可为字符串或输入项数组（见 [`ResponsesInputItem`]）。
    pub input: serde_json::Value,
    /// 系统级指令；缺省时表示未设置。
    #[serde(default)]
    pub instructions: Option<String>,
    /// 是否以 SSE 流式返回；缺省时按非流式处理。
    #[serde(default)]
    pub stream: bool,
    /// 工具定义列表，保持原始 JSON 结构透传。
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    /// 采样温度；缺省时使用模型默认值。
    #[serde(default)]
    pub temperature: Option<f64>,
    /// 最大输出 token 数；缺省时使用模型默认值。
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// 工具选择策略；缺省时按 auto 处理。
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    /// 是否允许并行工具调用；缺省时由上游决定。
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// 关联的上一次响应 ID，用于多轮对话续接；缺省时表示全新对话。
    #[serde(default)]
    pub previous_response_id: Option<String>,
}

/// Responses API 输入项，按 JSON 中的 `"type"` 字段做标签反序列化。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ResponsesInputItem {
    /// 普通消息项。
    #[serde(rename = "message")]
    Message {
        /// 发送方角色，如 `"user"`、`"assistant"`、`"system"`。
        role: String,
        /// 消息内容，可为字符串或内容段数组。
        content: serde_json::Value,
    },
    /// 历史中的函数调用项。
    #[serde(rename = "function_call")]
    FunctionCall {
        /// 调用唯一标识。
        call_id: String,
        /// 被调用函数名称。
        name: String,
        /// 函数入参，JSON 文本形式。
        arguments: String,
    },
    /// 函数调用的结果回传项。
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        /// 对应调用的唯一标识。
        call_id: String,
        /// 函数输出内容，可为字符串或结构化对象。
        output: serde_json::Value,
    },
}

/// Responses API 非流式响应体。
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesResponse {
    /// 本次响应的唯一 ID。
    pub id: String,
    /// 对象类型常量，固定为 `"response"`。
    pub object: String,
    /// 创建时间戳（Unix 秒）。
    pub created_at: u64,
    /// 响应模型标识。
    pub model: String,
    /// 输出项列表（消息 / 函数调用）。
    pub output: Vec<ResponseOutputItem>,
    /// token 用量统计。
    pub usage: ResponseUsage,
}

/// 响应输出项，兼容消息与函数调用两类形态（同一结构按 `type` 区分）。
#[derive(Debug, Clone, Serialize)]
pub struct ResponseOutputItem {
    /// 输出项类型，序列化时映射为 `"type"`，取值为 `"message"` 或 `"function_call"`。
    #[serde(rename = "type")]
    pub item_type: String,
    /// 输出项唯一 ID。
    pub id: String,
    /// 函数调用的 `call_id`；消息项为 `None`，序列化时跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// 项状态，如 `"completed"`。
    pub status: String,
    /// 角色，固定为 `"assistant"`。
    pub role: String,
    /// 函数名称；消息项为 `None`，序列化时跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 函数入参 JSON 文本；消息项为 `None`，序列化时跳过。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    /// 内容段列表；函数调用项序列化为空数组。
    pub content: Vec<ResponseContentPart>,
}

/// 响应中的单个内容段。
#[derive(Debug, Clone, Serialize)]
pub struct ResponseContentPart {
    /// 内容段类型，序列化时映射为 `"type"`，当前取值为 `"output_text"`。
    #[serde(rename = "type")]
    pub content_type: String,
    /// 文本内容。
    pub text: String,
    /// 注解列表（引用、出处等），当前恒为空数组。
    pub annotations: Vec<serde_json::Value>,
}

/// Responses API 的 token 用量统计。
#[derive(Debug, Clone, Serialize)]
pub struct ResponseUsage {
    /// 输入消耗的 token 数。
    pub input_tokens: u64,
    /// 输出消耗的 token 数。
    pub output_tokens: u64,
    /// 总 token 数（输入 + 输出）。
    pub total_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_output_item_function_call_serializes_with_call_id_and_arguments() {
        let item = ResponseOutputItem {
            item_type: "function_call".to_string(),
            id: "fc_1".to_string(),
            call_id: Some("call_abc".to_string()),
            status: "completed".to_string(),
            role: "assistant".to_string(),
            name: Some("Bash".to_string()),
            arguments: Some("{\"command\":\"ls\"}".to_string()),
            content: Vec::new(),
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["type"], "function_call");
        assert_eq!(v["call_id"], "call_abc");
        assert_eq!(v["name"], "Bash");
        assert_eq!(v["arguments"], "{\"command\":\"ls\"}");
        // function_call 项的 content 始终序列化为空数组
        assert_eq!(v["content"], serde_json::json!([]));
    }

    #[test]
    fn response_output_item_message_omits_function_call_fields() {
        let item = ResponseOutputItem {
            item_type: "message".to_string(),
            id: "msg_1".to_string(),
            call_id: None,
            status: "completed".to_string(),
            role: "assistant".to_string(),
            name: None,
            arguments: None,
            content: vec![ResponseContentPart {
                content_type: "output_text".to_string(),
                text: "hi".to_string(),
                annotations: Vec::new(),
            }],
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["type"], "message");
        assert!(v.get("call_id").is_none());
        assert!(v.get("name").is_none());
        assert!(v.get("arguments").is_none());
        assert_eq!(v["content"][0]["text"], "hi");
    }
}
