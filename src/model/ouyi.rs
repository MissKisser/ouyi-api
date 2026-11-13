//! 欧易 8.0（Tmpl）协议的报文结构定义。
//! 覆盖模板接口（模型列表 / 供应商列表 / 默认配置）、会话创建与聊天请求，
//! 以及 SSE 流式事件与用量统计结构，供欧易 8.0 客户端与模型目录构建使用。

use serde::{Deserialize, Serialize};

/// 欧易 8.0 模板接口的通用响应包装。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TmplResponse {
    /// 业务状态码，0 表示成功。
    #[serde(default)]
    pub code: i32,
    /// 状态描述信息。
    #[serde(default)]
    pub msg: String,
    /// 业务数据负载；接口异常或无数据时为 `None`。
    pub data: Option<TmplData>,
}

/// 模板接口的业务数据：模型列表、供应商列表与站点级默认配置。
#[derive(Debug, Clone, Deserialize)]
pub struct TmplData {
    /// 可用模型列表。
    pub models: Vec<TmplModel>,
    /// 供应商列表；缺省时为空。
    #[serde(default)]
    pub providers: Vec<TmplProvider>,
    /// 默认模型标识（完整名）。
    #[serde(rename = "defModel", default)]
    pub def_model: String,
    /// 默认聊天模型标识。
    #[serde(rename = "defaultChat", default)]
    pub default_chat: String,
    /// 站点功能开关，含义由上游约定；缺省时为 `false`。
    #[serde(default)]
    pub cm: bool,
    /// 标题生成相关的站点配置；当前流程未消费。
    #[serde(rename = "genLine", default)]
    pub gen_line: i32,
    /// 是否开启标题生成；缺省时为 `false`。
    #[serde(rename = "genTitle", default)]
    pub gen_title: bool,
    /// 单文件上传数量上限；缺省时为 0。
    #[serde(rename = "mFileCount", default)]
    pub m_file_count: i32,
    /// 单文件大小上限；缺省时为 0。
    #[serde(rename = "mFileSize", default)]
    pub m_file_size: i32,
    /// MCP 配置列表，当前流程仅解析占位、不消费具体内容。
    #[serde(default)]
    pub mcp: Vec<serde::de::IgnoredAny>,
}

/// 模板接口中的单个模型条目。
#[derive(Debug, Clone, Deserialize)]
pub struct TmplModel {
    /// 模型展示名。
    pub label: String,
    /// 模型完整标识（如 `claude_chat::claude-sonnet-4-5-vip`），请求时使用。
    pub value: String,
    /// 模型附加属性（能力位、供应商等）。
    pub attr: TmplModelAttr,
}

/// 模型条目的附加属性。
#[derive(Debug, Clone, Deserialize)]
pub struct TmplModelAttr {
    /// 模型能力位；缺省时按全关处理。
    #[serde(default)]
    pub capabilities: TmplCapabilities,
    /// 供应商标识 key；缺省时为空串，由调用方推断。
    #[serde(default)]
    pub provider_key: String,
    /// 供应商展示名称；缺省时为空串，由调用方补齐。
    #[serde(default)]
    pub provider_name: String,
    /// 积分 / 权益相关描述，当前流程未消费。
    #[serde(default)]
    pub integral: String,
    /// 模型类型标记；缺省时为空串。
    #[serde(rename = "modelType", default)]
    pub model_type: String,
}

/// 模型能力位集合，各字段缺省时均为 `false`。
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TmplCapabilities {
    /// 是否支持图片输入。
    #[serde(rename = "imageInput", default)]
    pub image_input: bool,
    /// 是否支持任意文件输入。
    #[serde(rename = "anyFile", default)]
    pub any_file: bool,
    /// 是否支持工具调用。
    #[serde(default)]
    pub tools: bool,
    /// 是否支持流式（SSE）输出。
    #[serde(default)]
    pub stream: bool,
    /// 是否支持系统角色消息。
    #[serde(rename = "systemRole", default)]
    pub system_role: bool,
    /// 是否支持扩展思考输出。
    #[serde(default)]
    pub thinking: bool,
    /// 是否由服务端自动检测思考内容。
    #[serde(rename = "autoDetectThinking", default)]
    pub auto_detect_thinking: bool,
}

/// 模板接口中的供应商条目。
#[derive(Debug, Clone, Deserialize)]
pub struct TmplProvider {
    /// 供应商标识 key，序列化时映射为 `"idKey"`。
    #[serde(rename = "idKey")]
    pub id_key: String,
    /// 供应商展示名称。
    pub name: String,
    /// 排序权重，数值越小越靠前。
    pub sort: i32,
}

/// 欧易 8.0 创建会话接口的响应包装。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionResponse {
    /// 业务状态码，0 表示成功。
    pub code: i32,
    /// 状态描述信息。
    pub msg: String,
    /// 会话数据；创建失败时为 `None`。
    pub data: Option<SessionData>,
}

/// 创建会话成功后返回的会话数据。
#[derive(Debug, Clone, Deserialize)]
pub struct SessionData {
    /// 会话 ID，后续聊天请求需携带。
    pub id: i64,
    /// 会话绑定的模型标识；缺省时为空串。
    #[serde(default)]
    pub model: String,
    /// 会话上下文轮数计数；缺省时为 0。
    #[serde(rename = "contextCount", default)]
    pub context_count: i32,
    /// 会话采样温度；缺省时为 0。
    #[serde(default)]
    pub temperature: f64,
}

/// 欧易 8.0 创建会话接口的请求体。
#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionRequest {
    /// 会话绑定的模型完整标识。
    pub model: String,
    /// 插件配置列表，当前恒为空数组。
    pub plugins: Vec<serde_json::Value>,
    /// MCP 配置列表，当前恒为空数组。
    pub mcp: Vec<serde_json::Value>,
    /// RAG 配置列表，当前恒为空数组。
    pub rags: Vec<serde_json::Value>,
}

impl CreateSessionRequest {
    /// 以指定模型创建会话请求，插件 / MCP / RAG 列表均置空。
    ///
    /// # 参数
    /// - `model`：会话绑定的模型完整标识。
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            plugins: Vec::new(),
            mcp: Vec::new(),
            rags: Vec::new(),
        }
    }
}

/// 欧易 8.0 聊天接口的请求体。
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    /// 本轮用户输入文本。
    pub text: String,
    /// 目标会话 ID，序列化时映射为 `"sessionId"`。
    #[serde(rename = "sessionId")]
    pub session_id: i64,
    /// 随消息上传的文件列表，当前恒为空数组。
    pub files: Vec<serde_json::Value>,
    /// 是否开启扩展思考。
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

/// 欧易 8.0 SSE 流中的单个事件。
#[derive(Debug, Clone, Deserialize)]
pub struct OuyiSseEvent {
    /// 事件 ID；当前流程未消费，保留以兼容协议字段。
    #[allow(dead_code)]
    pub id: Option<String>,
    /// 事件类型（如 `message`、`usage`、`end` 等，由上游约定）。
    #[serde(rename = "type", default)]
    pub event_type: String,
    /// 事件数据负载；无数据事件为 `None`。
    pub data: Option<serde_json::Value>,
    /// 错误描述；正常事件为 `None`。
    pub err: Option<String>,
    /// 错误码；正常事件为 `None`。
    #[allow(dead_code)]
    pub code: Option<i32>,
}

/// 欧易 8.0 返回的 token 用量统计。
#[derive(Debug, Clone, Deserialize)]
pub struct OuyiUsage {
    /// 输入消耗的 token 数，序列化时映射为 `"promptTokens"`；缺省时为 0。
    #[serde(rename = "promptTokens", default)]
    pub prompt_tokens: u64,
    /// 输出消耗的 token 数，序列化时映射为 `"completionTokens"`；缺省时为 0。
    #[serde(rename = "completionTokens", default)]
    pub completion_tokens: u64,
}
