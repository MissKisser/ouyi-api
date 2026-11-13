//! 欧易 7.0 协议的报文结构定义。
//! 覆盖通用响应包装、用户信息（订阅权限）、模型枚举、话题创建与消息发送
//! 等请求/响应结构，供欧易 7.0 客户端与模型目录构建使用。

use serde::{Deserialize, Serialize};

/// 欧易 7.0 API 通用响应包装。
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7Envelope<T> {
    /// 业务状态码，200 表示成功。
    pub code: i32,
    /// 业务数据负载；失败或无数据时为 `None`。
    pub result: Option<T>,
    /// 状态描述信息；缺省时为空串。
    #[serde(default)]
    pub message: String,
    /// 响应类型标记；当前流程未消费，保留以兼容协议字段。
    #[serde(default)]
    #[allow(dead_code)]
    pub r#type: Option<String>,
}

/// memberInfo 接口返回结果。
#[derive(Debug, Clone, Deserialize)]
pub struct MemberInfoResult {
    /// 用户组信息；无分组数据时为 `None`。
    #[serde(rename = "groupInfo")]
    pub group_info: Option<GroupInfo>,
}

/// 用户组信息。
#[derive(Debug, Clone, Deserialize)]
pub struct GroupInfo {
    /// 用户在该组下拥有的权限条目列表；缺省时为空。
    #[serde(default)]
    pub privileges: Vec<Privilege>,
}

/// 用户权限信息：记录该用户可用的两类模型订阅。
#[derive(Debug, Clone, Deserialize)]
pub struct Privilege {
    /// 自定义模型名列表（如 `"deepseek-chat"`），路由时走 `roleId=1` + `params.model=模型名`。
    #[serde(rename = "customAIModels", default)]
    pub custom_ai_models: Vec<String>,
    /// 标准模型的数字 value 列表，对应 sysEnum 里的 value，路由时直接作为 roleId 使用
    /// （如 gpt-4o value=35 → roleId=35；customAIModels 则走 roleId=1+params.model）。
    #[serde(rename = "aiModels", default)]
    pub ai_models: Vec<i64>,
}

/// 系统枚举项（模型列表）。
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7EnumItem {
    /// 人类可读的模型描述名（如 `"gpt-4o"`），作对外 id 与展示名。
    pub describe: String,
    /// 后端内部枚举 key（如 `"ChatGPT4o"`），不对外暴露。
    pub name: String,
    /// 模型的数字 value，作为 roleId 路由；保持原始 JSON 以兼容数字/字符串形式。
    pub value: serde_json::Value,
}

/// 创建话题请求。
#[derive(Debug, Clone, Serialize)]
pub struct Ouyi7CreateTopicRequest {
    /// 话题 ID，新建时传 0 由后端分配。
    pub id: i64,
    /// 模型路由 ID，序列化时映射为 `"roleId"`（custom 模型传 1，标准模型传枚举 value）。
    #[serde(rename = "roleId")]
    pub role_id: i64,
    /// 话题标题。
    pub title: String,
    /// 是否锁定话题，序列化时映射为 `"isLock"`。
    #[serde(rename = "isLock")]
    pub is_lock: bool,
    /// 话题级系统消息，序列化时映射为 `"systemMessage"`。
    #[serde(rename = "systemMessage")]
    pub system_message: String,
    /// 附加参数，JSON 文本形式（如携带自定义模型名）。
    pub params: String,
}

/// 创建话题返回结果。
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7TopicResult {
    /// 后端分配的话题 ID。
    pub id: i64,
}

/// 话题内的一条历史消息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ouyi7HistoryMessage {
    /// 发送方角色，如 `"user"`、`"assistant"`、`"system"`。
    pub role: String,
    /// 消息文本内容。
    pub content: String,
    /// 消息附带的文件列表；缺省时为空数组。
    #[serde(rename = "contentFiles", default)]
    pub content_files: Vec<serde_json::Value>,
}

/// 发送消息请求。
#[derive(Debug, Clone, Serialize)]
pub struct Ouyi7ChatMessageRequest {
    /// 目标话题 ID，序列化时映射为 `"topicId"`。
    #[serde(rename = "topicId")]
    pub topic_id: i64,
    /// 随请求携带的上下文历史消息列表。
    pub messages: Vec<Ouyi7HistoryMessage>,
    /// 本轮用户输入文本。
    pub content: String,
    /// 本轮消息附带的文件列表，序列化时映射为 `"contentFiles"`。
    #[serde(rename = "contentFiles")]
    pub content_files: Vec<serde_json::Value>,
}

/// 发送消息第一阶段返回（`[userMsgId, aiMsgId]`）。
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7MessagePhase1Result(
    /// 元组内容：元素 0 为用户消息 ID，元素 1 为 AI 消息 ID；
    /// 反序列化要求数组至少含 2 个元素，否则报错。
    #[serde(deserialize_with = "deserialize_message_ids")] pub (i64, i64),
);

fn deserialize_message_ids<'de, D>(deserializer: D) -> Result<(i64, i64), D::Error>
where
    D: serde::Deserializer<'de>,
{
    let arr: Vec<i64> = Vec::deserialize(deserializer)?;
    if arr.len() >= 2 {
        Ok((arr[0], arr[1]))
    } else {
        Err(serde::de::Error::custom(
            "expected array with at least 2 elements",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ouyi7_envelope_success() {
        let json = r#"{"code":200,"result":{"groupInfo":{"privileges":[{"customAIModels":["gpt-4","claude-3"]}]}}}"#;
        let envelope: Ouyi7Envelope<MemberInfoResult> = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.code, 200);
        assert!(envelope.result.is_some());
        let result = envelope.result.unwrap();
        assert!(result.group_info.is_some());
        let group = result.group_info.unwrap();
        assert_eq!(group.privileges.len(), 1);
        assert_eq!(
            group.privileges[0].custom_ai_models,
            vec!["gpt-4", "claude-3"]
        );
    }

    #[test]
    fn test_ouyi7_envelope_error_code() {
        let json = r#"{"code":401,"result":null}"#;
        let envelope: Ouyi7Envelope<MemberInfoResult> = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.code, 401);
        assert!(envelope.result.is_none());
    }

    #[test]
    fn test_ouyi7_enum_item() {
        let json = r#"{"describe":"GPT-4","name":"gpt-4","value":"gpt-4-v1"}"#;
        let item: Ouyi7EnumItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.describe, "GPT-4");
        assert_eq!(item.name, "gpt-4");
        assert_eq!(
            item.value,
            serde_json::Value::String("gpt-4-v1".to_string())
        );
    }

    #[test]
    fn test_ouyi7_history_message() {
        let msg = Ouyi7HistoryMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
            content_files: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Hello\""));
        assert!(json.contains("\"contentFiles\":[]"));
    }

    #[test]
    fn test_ouyi7_history_message_with_files() {
        let json = r#"{"role":"assistant","content":"Response","contentFiles":[{"type":"file","url":"http://example.com/file.pdf"}]}"#;
        let msg: Ouyi7HistoryMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "Response");
        assert_eq!(msg.content_files.len(), 1);
    }

    #[test]
    fn test_ouyi7_chat_message_request() {
        let req = Ouyi7ChatMessageRequest {
            topic_id: 12345,
            messages: vec![
                Ouyi7HistoryMessage {
                    role: "system".to_string(),
                    content: "You are helpful".to_string(),
                    content_files: vec![],
                },
                Ouyi7HistoryMessage {
                    role: "user".to_string(),
                    content: "Hi".to_string(),
                    content_files: vec![],
                },
            ],
            content: "Hello".to_string(),
            content_files: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"topicId\":12345"));
        assert!(json.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_ouyi7_message_phase1_result() {
        let json = r#"[1001,2002]"#;
        let result: Ouyi7MessagePhase1Result = serde_json::from_str(json).unwrap();
        assert_eq!(result.0, (1001, 2002));
    }

    #[test]
    fn test_ouyi7_topic_result() {
        let json = r#"{"id":2188454897517829}"#;
        let result: Ouyi7TopicResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.id, 2188454897517829);
    }

    #[test]
    fn test_ouyi7_create_topic_request() {
        let req = Ouyi7CreateTopicRequest {
            id: 0,
            role_id: 1,
            title: "New Chat".to_string(),
            is_lock: false,
            system_message: "You are helpful".to_string(),
            params: "{}".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"roleId\":1"));
        assert!(json.contains("\"isLock\":false"));
        assert!(json.contains("\"systemMessage\":\"You are helpful\""));
    }
}
