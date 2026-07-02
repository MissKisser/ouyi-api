use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub temperature: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    #[allow(dead_code)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: ChunkDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallFunctionDelta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallFunctionDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ResponseChoice>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseChoice {
    pub index: u32,
    pub message: ResponseMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: serde_json::Value,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesResponse {
    pub id: String,
    pub object: String,
    pub created_at: u64,
    pub model: String,
    pub output: Vec<ResponseOutputItem>,
    pub usage: ResponseUsage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub item_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub status: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    pub content: Vec<ResponseContentPart>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseContentPart {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
    pub annotations: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        // content should be skipped or empty array
        assert!(v.get("content").is_none() || v["content"].is_array());
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
