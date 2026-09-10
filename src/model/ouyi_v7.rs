use serde::{Deserialize, Serialize};

/// 7.0 API 通用响应包装
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7Envelope<T> {
    pub code: i32,
    pub result: Option<T>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub r#type: Option<String>,
}

/// memberInfo 接口返回结果
#[derive(Debug, Clone, Deserialize)]
pub struct MemberInfoResult {
    #[serde(rename = "groupInfo")]
    pub group_info: Option<GroupInfo>,
}

/// 用户组信息
#[derive(Debug, Clone, Deserialize)]
pub struct GroupInfo {
    #[serde(default)]
    pub privileges: Vec<Privilege>,
}

/// 用户权限信息
#[derive(Debug, Clone, Deserialize)]
pub struct Privilege {
    #[serde(rename = "customAIModels", default)]
    pub custom_ai_models: Vec<String>,
    /// 标准模型（数字 value 数组，对应 sysEnum 里的 value，作为 roleId 路由）
    /// 实地 JWT 探测确认：customAIModels 走 roleId=1+params.model，
    /// aiModels 走 roleId=value（如 gpt-4o value=35 → roleId=35）。
    #[serde(rename = "aiModels", default)]
    pub ai_models: Vec<i64>,
}

/// 系统枚举项（模型列表）
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7EnumItem {
    pub describe: String,
    pub name: String,
    pub value: serde_json::Value,
}

/// 创建话题请求
#[derive(Debug, Clone, Serialize)]
pub struct Ouyi7CreateTopicRequest {
    pub id: i64,
    #[serde(rename = "roleId")]
    pub role_id: i64,
    pub title: String,
    #[serde(rename = "isLock")]
    pub is_lock: bool,
    #[serde(rename = "systemMessage")]
    pub system_message: String,
    pub params: String,
}

/// 创建话题返回结果
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7TopicResult {
    pub id: i64,
}

/// 历史消息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ouyi7HistoryMessage {
    pub role: String,
    pub content: String,
    #[serde(rename = "contentFiles", default)]
    pub content_files: Vec<serde_json::Value>,
}

/// 发送消息请求
#[derive(Debug, Clone, Serialize)]
pub struct Ouyi7ChatMessageRequest {
    #[serde(rename = "topicId")]
    pub topic_id: i64,
    pub messages: Vec<Ouyi7HistoryMessage>,
    pub content: String,
    #[serde(rename = "contentFiles")]
    pub content_files: Vec<serde_json::Value>,
}

/// 发送消息第一阶段返回（[userMsgId, aiMsgId]）
#[derive(Debug, Clone, Deserialize)]
pub struct Ouyi7MessagePhase1Result(
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
