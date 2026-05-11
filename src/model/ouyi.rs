use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct TmplResponse {
    pub code: i32,
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
pub struct ChatRequest {
    pub text: String,
    #[serde(rename = "sessionId")]
    pub session_id: i64,
    pub files: Vec<serde_json::Value>,
    pub thinking: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OuyiSseEvent {
    #[allow(dead_code)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Option<serde_json::Value>,
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
