use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TmplResponse {
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub msg: String,
    pub data: Option<TmplData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TmplData {
    pub models: Vec<TmplModel>,
    #[serde(default)]
    pub providers: Vec<TmplProvider>,
    #[serde(rename = "defModel", default)]
    pub def_model: String,
    #[serde(rename = "defaultChat", default)]
    pub default_chat: String,
    #[serde(default)]
    pub cm: bool,
    #[serde(rename = "genLine", default)]
    pub gen_line: i32,
    #[serde(rename = "genTitle", default)]
    pub gen_title: bool,
    #[serde(rename = "mFileCount", default)]
    pub m_file_count: i32,
    #[serde(rename = "mFileSize", default)]
    pub m_file_size: i32,
    #[serde(default)]
    pub mcp: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TmplModel {
    pub label: String,
    pub value: String,
    pub attr: TmplModelAttr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TmplModelAttr {
    #[serde(default)]
    pub capabilities: TmplCapabilities,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub integral: String,
    #[serde(rename = "modelType", default)]
    pub model_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TmplCapabilities {
    #[serde(rename = "imageInput", default)]
    pub image_input: bool,
    #[serde(rename = "anyFile", default)]
    pub any_file: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(rename = "systemRole", default)]
    pub system_role: bool,
    #[serde(default)]
    pub thinking: bool,
    #[serde(rename = "autoDetectThinking", default)]
    pub auto_detect_thinking: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TmplProvider {
    #[serde(rename = "idKey")]
    pub id_key: String,
    pub name: String,
    pub sort: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionResponse {
    pub code: i32,
    pub msg: String,
    pub data: Option<SessionData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionData {
    pub id: i64,
    #[serde(default)]
    pub model: String,
    #[serde(rename = "contextCount", default)]
    pub context_count: i32,
    #[serde(default)]
    pub temperature: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionRequest {
    pub model: String,
    pub plugins: Vec<serde_json::Value>,
    pub mcp: Vec<serde_json::Value>,
    pub rags: Vec<serde_json::Value>,
}

impl CreateSessionRequest {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            plugins: Vec::new(),
            mcp: Vec::new(),
            rags: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub text: String,
    #[serde(rename = "sessionId")]
    pub session_id: i64,
    pub files: Vec<serde_json::Value>,
    pub thinking: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_session_request_matches_ouyi_frontend_shape() {
        let req = CreateSessionRequest::new("claude_chat::claude-sonnet-4-6");
        let value = serde_json::to_value(req).unwrap();

        assert_eq!(value["model"], "claude_chat::claude-sonnet-4-6");
        assert_eq!(value["plugins"], serde_json::json!([]));
        assert_eq!(value["mcp"], serde_json::json!([]));
        assert_eq!(value["rags"], serde_json::json!([]));
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OuyiSseEvent {
    #[allow(dead_code)]
    pub id: Option<String>,
    #[serde(rename = "type", default)]
    pub event_type: String,
    pub data: Option<serde_json::Value>,
    pub err: Option<String>,
    #[allow(dead_code)]
    pub code: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OuyiUsage {
    #[serde(rename = "promptTokens", default)]
    pub prompt_tokens: u64,
    #[serde(rename = "completionTokens", default)]
    pub completion_tokens: u64,
}
