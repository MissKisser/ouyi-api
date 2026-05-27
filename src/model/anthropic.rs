use serde::{Deserialize, Serialize};

// ── Request ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemContent>,
    pub max_tokens: u64,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicMessage {
    pub role: String,
    #[serde(deserialize_with = "deserialize_content")]
    pub content: AnthropicContent,
}

#[derive(Debug, Clone)]
pub enum AnthropicContent {
    Text(String),
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

#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
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
            } => serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content
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
                Ok(ContentBlock::ToolResult {
                    tool_use_id,
                    content: content_str,
                })
            }
            _ => Err(de::Error::custom(format!(
                "unknown content block type: {}",
                type_str
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemBlock {
    pub r#type: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "tool")]
    Tool { name: String },
}

impl ToolChoice {
    pub fn is_forced(&self) -> bool {
        matches!(self, ToolChoice::Any | ToolChoice::Tool { .. })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ThinkingConfig {
    #[serde(rename = "enabled")]
    Enabled { budget_tokens: u32 },
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "adaptive")]
    Adaptive,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesResponse {
    pub id: String,
    pub r#type: &'static str,
    pub role: &'static str,
    pub model: String,
    pub content: Vec<ResponseContentBlock>,
    pub stop_reason: String,
    pub usage: ResponseUsage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ResponseContentBlock {
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

// ── Streaming SSE Events ───────────────────────────────────────────────────

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

#[derive(Debug, Clone)]
pub enum StreamEvent {
    MessageStart {
        message: MessageStartInfo,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockStart,
    },
    ContentBlockDelta {
        index: u32,
        delta: DeltaContent,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaInfo,
        usage: ResponseUsage,
    },
    MessageStop,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageStartInfo {
    pub r#type: &'static str,
    pub message: MessageStartMessage,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageStartMessage {
    pub id: String,
    pub r#type: &'static str,
    pub role: &'static str,
    pub model: String,
    pub content: Vec<serde_json::Value>,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: ResponseUsage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlockStart {
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DeltaContent {
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "signature_delta")]
    SignatureDelta { signature: String },
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageDeltaInfo {
    pub stop_reason: String,
    pub stop_sequence: Option<String>,
}
