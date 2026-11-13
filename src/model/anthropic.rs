//! Anthropic Messages API 的数据模型定义。
//! 涵盖 `/v1/messages` 的请求与响应结构、消息内容块、工具调用与思考配置，
//! 以及流式（SSE）事件的序列化结构，供协议接入层与协议转换层使用。

use serde::{Deserialize, Serialize};

// ── Request ────────────────────────────────────────────────────────────────

/// Anthropic Messages 接口（`/v1/messages`）的请求体。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MessagesRequest {
    /// 目标模型标识，例如 `"claude-sonnet-4-5"`。
    pub model: String,
    /// 对话历史消息列表，按时间先后排列。
    pub messages: Vec<AnthropicMessage>,
    /// 系统提示词，可为纯文本或系统内容块数组；缺省时表示未设置。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemContent>,
    /// 单次响应允许生成的最大 token 数，协议必填。
    pub max_tokens: u64,
    /// 是否以 SSE 流式返回；缺省时按非流式处理。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// 本次请求可调用的工具定义列表；缺省时表示不提供工具。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    /// 采样温度，取值越高输出越随机；缺省时使用模型默认值。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// 核采样概率阈值，仅从累计概率不超过该阈值的候选中采样；缺省时使用模型默认值。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// 每步仅保留概率最高的前 k 个候选 token；缺省时使用模型默认值。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// 命中其中任一序列即停止生成；缺省时表示不设置停止序列。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// 扩展思考配置，控制是否启用推理及推理预算；缺省时使用模型默认行为。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// 工具选择策略（自动 / 强制任一 / 指定工具）；缺省时按 auto 处理。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

/// 对话中的单条消息，由发送方角色与消息内容组成。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicMessage {
    /// 发送方角色，取值为 `"user"` 或 `"assistant"`。
    pub role: String,
    /// 消息内容，协议上接受纯文本字符串或内容块数组两种形式。
    #[serde(deserialize_with = "deserialize_content")]
    pub content: AnthropicContent,
}

/// 消息内容的统一表示。
/// 反序列化时兼容字符串与内容块数组两种协议形式，序列化时按原形态输出。
#[derive(Debug, Clone)]
pub enum AnthropicContent {
    /// 纯文本形式的消息内容。
    Text(String),
    /// 结构化内容块（文本 / 思考 / 工具调用 / 工具结果）列表。
    Blocks(Vec<ContentBlock>),
}

impl Serialize for AnthropicContent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AnthropicContent::Text(text) => serializer.serialize_str(text),
            AnthropicContent::Blocks(blocks) => blocks.serialize(serializer),
        }
    }
}

fn deserialize_content<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<AnthropicContent, D::Error> {
    use serde::de;
    let value = serde_json::Value::deserialize(d)?;
    match value {
        serde_json::Value::String(s) => Ok(AnthropicContent::Text(s)),
        serde_json::Value::Array(arr) => {
            let blocks: Result<Vec<ContentBlock>, _> =
                serde_json::from_value(serde_json::Value::Array(arr));
            blocks
                .map(AnthropicContent::Blocks)
                .map_err(de::Error::custom)
        }
        _ => Err(de::Error::custom(
            "content must be a string or array of blocks",
        )),
    }
}

/// 消息内容块，覆盖文本、思考、工具调用与工具结果四类。
/// 序列化与反序列化均为手工实现，按 JSON 中的 `"type"` 字段区分类型。
#[derive(Debug, Clone)]
pub enum ContentBlock {
    /// 纯文本块。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 模型扩展思考块。
    Thinking {
        /// 思考正文。
        thinking: String,
        /// 思考内容签名，用于校验回传的思考未被篡改；可为空。
        signature: Option<String>,
    },
    /// 模型发起的工具调用块。
    ToolUse {
        /// 本次调用的唯一标识，工具结果需回填该 ID。
        id: String,
        /// 被调用工具的名称。
        name: String,
        /// 工具入参，结构由对应工具的 schema 约定。
        input: serde_json::Value,
    },
    /// 客户端回传的工具执行结果块。
    ToolResult {
        /// 对应 ToolUse 块的调用标识。
        tool_use_id: String,
        /// 工具输出的文本内容；协议上的数组形式在解析时已归一为换行拼接的字符串。
        content: String,
        /// 工具是否执行失败；`true` 表示结果为错误。
        is_error: bool,
    },
}

impl Serialize for ContentBlock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ContentBlock::Text { text } => serde_json::json!({
                "type": "text",
                "text": text
            })
            .serialize(serializer),
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                let mut value = serde_json::json!({
                    "type": "thinking",
                    "thinking": thinking
                });
                if let Some(signature) = signature {
                    value["signature"] = serde_json::Value::String(signature.clone());
                }
                value.serialize(serializer)
            }
            ContentBlock::ToolUse { id, name, input } => serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            })
            .serialize(serializer),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error
            })
            .serialize(serializer),
        }
    }
}

fn extract_tool_result_text(content: Option<&serde_json::Value>) -> String {
    match content {
        None => String::new(),
        Some(c) if c.is_string() => c.as_str().unwrap_or("").to_string(),
        Some(serde_json::Value::Array(arr)) => {
            let texts: Vec<String> = arr
                .iter()
                .filter_map(|item| {
                    if item.is_string() {
                        item.as_str().map(|s| s.to_string())
                    } else if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                        item.get("text")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            texts.join("\n")
        }
        Some(c) => {
            if let Some(s) = c.as_str() {
                s.to_string()
            } else {
                serde_json::to_string(c).unwrap_or_default()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de;
        let mut value = serde_json::Value::deserialize(d)?;
        let type_str = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match type_str {
            "text" => {
                let text = value.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Ok(ContentBlock::Text {
                    text: text.to_string(),
                })
            }
            "tool_use" => {
                let id = value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = value
                    .get_mut("input")
                    .map(serde_json::Value::take)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                Ok(ContentBlock::ToolUse { id, name, input })
            }
            "thinking" => {
                let thinking = value
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let signature = value
                    .get("signature")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Ok(ContentBlock::Thinking {
                    thinking,
                    signature,
                })
            }
            "tool_result" => {
                let tool_use_id = value
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content_str = extract_tool_result_text(value.get("content"));
                let is_error = value
                    .get("is_error")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                Ok(ContentBlock::ToolResult {
                    tool_use_id,
                    content: content_str,
                    is_error,
                })
            }
            _ => Err(de::Error::custom(format!(
                "unknown content block type: {}",
                type_str
            ))),
        }
    }
}

/// 系统提示词内容，兼容纯文本与系统内容块数组两种协议形式。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SystemContent {
    /// 纯文本系统提示词。
    Text(String),
    /// 系统内容块数组，可携带缓存控制等附加属性。
    Blocks(Vec<SystemBlock>),
}

/// 系统提示词中的单个内容块。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemBlock {
    /// 块类型，当前取值为 `"text"`。
    pub r#type: String,
    /// 块的文本内容。
    pub text: String,
}

/// 提供给模型的工具定义。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicTool {
    /// 工具名称，模型通过该名称发起调用。
    pub name: String,
    /// 工具功能描述，帮助模型判断何时调用；缺省时为空字符串。
    #[serde(default)]
    pub description: String,
    /// 工具入参的 JSON Schema 定义。
    pub input_schema: serde_json::Value,
}

/// 工具选择策略，按 JSON 中的 `"type"` 字段做标签序列化。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ToolChoice {
    /// 由模型自行决定是否调用工具。
    #[serde(rename = "auto")]
    Auto,
    /// 强制模型必须调用任一可用工具。
    #[serde(rename = "any")]
    Any,
    /// 强制模型调用指定名称的工具。
    #[serde(rename = "tool")]
    Tool {
        /// 强制调用的工具名称。
        name: String,
    },
}

impl ToolChoice {
    /// 判断该策略是否强制模型产生工具调用（any 或指定 tool 时为强制）。
    ///
    /// 返回 `true` 表示本轮不允许输出纯文本。
    pub fn is_forced(&self) -> bool {
        matches!(self, ToolChoice::Any | ToolChoice::Tool { .. })
    }
}

/// 扩展思考配置，按 JSON 中的 `"type"` 字段做标签序列化。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ThinkingConfig {
    /// 启用扩展思考，并指定思考阶段可消耗的 token 预算。
    #[serde(rename = "enabled")]
    Enabled {
        /// 思考阶段允许消耗的最大 token 数。
        budget_tokens: u32,
    },
    /// 关闭扩展思考。
    #[serde(rename = "disabled")]
    Disabled,
    /// 由模型按问题复杂度自适应决定思考深度。
    #[serde(rename = "adaptive")]
    Adaptive,
}

/// Anthropic Messages 接口的非流式响应体。
#[derive(Debug, Clone, Serialize)]
pub struct MessagesResponse {
    /// 本次响应的唯一消息 ID。
    pub id: String,
    /// 对象类型常量，固定为 `"message"`。
    pub r#type: &'static str,
    /// 角色常量，固定为 `"assistant"`。
    pub role: &'static str,
    /// 实际响应的模型标识。
    pub model: String,
    /// 响应内容块列表（思考 / 文本 / 工具调用）。
    pub content: Vec<ResponseContentBlock>,
    /// 停止原因，如 `"end_turn"`、`"max_tokens"`、`"tool_use"`。
    pub stop_reason: String,
    /// 本次请求的 token 用量统计。
    pub usage: ResponseUsage,
}

/// 响应内容块，仅包含模型输出会产生的三类。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseContentBlock {
    /// 扩展思考块。
    #[serde(rename = "thinking")]
    Thinking {
        /// 思考正文。
        thinking: String,
        /// 思考内容签名。
        signature: String,
    },
    /// 文本输出块。
    #[serde(rename = "text")]
    Text {
        /// 文本内容。
        text: String,
    },
    /// 工具调用块。
    #[serde(rename = "tool_use")]
    ToolUse {
        /// 本次调用的唯一标识。
        id: String,
        /// 被调用工具的名称。
        name: String,
        /// 工具入参。
        input: serde_json::Value,
    },
}

/// token 用量统计。
#[derive(Debug, Clone, Serialize)]
pub struct ResponseUsage {
    /// 输入消耗的 token 数。
    pub input_tokens: u64,
    /// 输出消耗的 token 数。
    pub output_tokens: u64,
}

// ── Streaming SSE Events ───────────────────────────────────────────────────

/// 将流式事件序列化为符合 Anthropic SSE 规范的字节帧。
///
/// 每个事件输出为 `event: <类型>\ndata: <JSON>\n\n` 的形式；内部 JSON
/// 序列化失败时对应 `data` 段落为空串，不会 panic。
///
/// # 参数
/// - `event`：待序列化的流式事件。
///
/// # 返回
/// 可直接写入响应体的 SSE 帧字节。
pub fn serialize_stream_event(event: &StreamEvent) -> bytes::Bytes {
    let s = match event {
        StreamEvent::MessageStart { message } => {
            format!(
                "event: message_start\ndata: {}\n\n",
                serde_json::to_string(message).unwrap_or_default()
            )
        }
        StreamEvent::ContentBlockStart {
            index,
            content_block,
        } => {
            format!(
                "event: content_block_start\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": content_block,
                }))
                .unwrap_or_default()
            )
        }
        StreamEvent::ContentBlockDelta { index, delta } => {
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": delta,
                }))
                .unwrap_or_default()
            )
        }
        StreamEvent::ContentBlockStop { index } => {
            format!(
                "event: content_block_stop\ndata: {}\n\n",
                serde_json::to_string(&serde_json::json!({
                    "type": "content_block_stop",
                    "index": index
                }))
                .unwrap_or_default()
            )
        }
        StreamEvent::MessageDelta { delta, usage } => {
            let mut obj = serde_json::json!({
                "type": "message_delta",
                "delta": delta,
            });
            if usage.output_tokens > 0 {
                obj["usage"] = serde_json::json!({"output_tokens": usage.output_tokens});
            }
            format!(
                "event: message_delta\ndata: {}\n\n",
                serde_json::to_string(&obj).unwrap_or_default()
            )
        }
        StreamEvent::MessageStop => {
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()
        }
    };
    bytes::Bytes::from(s)
}

/// 流式响应事件的内部表示，经 [`serialize_stream_event`] 转为 SSE 帧下发。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 流开始事件，携带完整消息头信息。
    MessageStart {
        /// 消息头（消息 ID、模型、初始用量等）。
        message: MessageStartInfo,
    },
    /// 内容块开始事件。
    ContentBlockStart {
        /// 块在消息内容列表中的下标。
        index: u32,
        /// 块的初始内容（类型与起始字段）。
        content_block: ContentBlockStart,
    },
    /// 内容块增量事件。
    ContentBlockDelta {
        /// 块下标，与对应 ContentBlockStart 一致。
        index: u32,
        /// 增量内容（文本 / 思考 / 签名 / 工具参数片段）。
        delta: DeltaContent,
    },
    /// 内容块结束事件。
    ContentBlockStop {
        /// 结束的块下标。
        index: u32,
    },
    /// 消息级增量事件，携带停止原因与累计用量。
    MessageDelta {
        /// 停止原因与停止序列信息。
        delta: MessageDeltaInfo,
        /// 截至目前的用量统计。
        usage: ResponseUsage,
    },
    /// 消息结束事件，无附加数据。
    MessageStop,
}

/// `message_start` 事件的数据负载。
#[derive(Debug, Clone, Serialize)]
pub struct MessageStartInfo {
    /// 事件类型常量，固定为 `"message_start"`。
    pub r#type: &'static str,
    /// 消息头信息。
    pub message: MessageStartMessage,
}

/// 流式消息开头的消息体描述。
#[derive(Debug, Clone, Serialize)]
pub struct MessageStartMessage {
    /// 消息唯一 ID。
    pub id: String,
    /// 对象类型常量，固定为 `"message"`。
    pub r#type: &'static str,
    /// 角色常量，固定为 `"assistant"`。
    pub role: &'static str,
    /// 响应模型标识。
    pub model: String,
    /// 初始内容列表，流开始时恒为空数组。
    pub content: Vec<serde_json::Value>,
    /// 停止原因，流开始时尚未确定，恒为 `None`。
    pub stop_reason: Option<String>,
    /// 停止序列，流开始时尚未确定，恒为 `None`。
    pub stop_sequence: Option<String>,
    /// 初始用量统计（仅含输入 token）。
    pub usage: ResponseUsage,
}

/// 内容块开始事件的块描述，按 JSON 中的 `"type"` 字段做标签序列化。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlockStart {
    /// 思考块开始。
    #[serde(rename = "thinking")]
    Thinking {
        /// 思考正文（开始时通常为空）。
        thinking: String,
        /// 思考签名（开始时通常为空）。
        signature: String,
    },
    /// 文本块开始。
    #[serde(rename = "text")]
    Text {
        /// 文本内容（开始时通常为空）。
        text: String,
    },
    /// 工具调用块开始。
    #[serde(rename = "tool_use")]
    ToolUse {
        /// 本次调用的唯一标识。
        id: String,
        /// 被调用工具的名称。
        name: String,
        /// 工具入参（开始时通常为空对象）。
        input: serde_json::Value,
    },
}

/// 内容块增量事件的增量负载，按 JSON 中的 `"type"` 字段做标签序列化。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DeltaContent {
    /// 思考正文增量。
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        /// 本次新增的思考文本片段。
        thinking: String,
    },
    /// 思考签名增量。
    #[serde(rename = "signature_delta")]
    SignatureDelta {
        /// 本次新增的签名字符串。
        signature: String,
    },
    /// 文本增量。
    #[serde(rename = "text_delta")]
    TextDelta {
        /// 本次新增的文本片段。
        text: String,
    },
    /// 工具入参 JSON 片段增量，客户端需按顺序拼接后整体解析。
    #[serde(rename = "input_json_delta")]
    InputJsonDelta {
        /// 本次新增的部分 JSON 字符串。
        partial_json: String,
    },
}

/// `message_delta` 事件中的停止信息。
#[derive(Debug, Clone, Serialize)]
pub struct MessageDeltaInfo {
    /// 最终停止原因，如 `"end_turn"`、`"max_tokens"`、`"tool_use"`。
    pub stop_reason: String,
    /// 触发停止的停止序列；未命中时为 `None`。
    pub stop_sequence: Option<String>,
}
