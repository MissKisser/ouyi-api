use crate::error::AppError;
use crate::model::ouyi_v7::{
    MemberInfoResult, Ouyi7ChatMessageRequest, Ouyi7CreateTopicRequest, Ouyi7EnumItem,
    Ouyi7Envelope, Ouyi7MessagePhase1Result, Ouyi7TopicResult,
};
use reqwest::Client;

/// 7.0 HTTP 客户端
#[derive(Clone)]
pub struct Ouyi7HttpClient {
    http_client: Client,
    base_url: String,
    chatapi_prefix: String,
    jwt_token: String,
}

impl Ouyi7HttpClient {
    pub fn new(
        http_client: Client,
        base_url: String,
        chatapi_prefix: String,
        jwt_token: String,
    ) -> Self {
        Self {
            http_client,
            base_url,
            chatapi_prefix,
            jwt_token,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 验证 JWT token
    pub async fn verify_token(&self) -> Result<bool, AppError> {
        let result = self.fetch_member_info().await?;
        Ok(result.group_info.is_some())
    }

    /// 获取用户信息
    pub async fn fetch_member_info(&self) -> Result<MemberInfoResult, AppError> {
        let url = format!(
            "{}{}{}",
            self.base_url, self.chatapi_prefix, "/auth/memberInfo"
        );
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .header("Accept", "application/json, text/plain, */*")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(AppError::Upstream(format!(
                "memberInfo failed: status {}",
                resp.status()
            )));
        }

        let envelope: Ouyi7Envelope<MemberInfoResult> = resp.json().await?;
        if envelope.code != 200 {
            return Err(AppError::Upstream(format!(
                "memberInfo code: {}",
                envelope.code
            )));
        }

        envelope
            .result
            .ok_or_else(|| AppError::Upstream("memberInfo result is null".to_string()))
    }

    /// 获取系统模型枚举
    pub async fn fetch_model_enum(&self) -> Result<Vec<Ouyi7EnumItem>, AppError> {
        let url = format!(
            "{}{}",
            self.base_url, "/api/sysEnum/enumDataList?EnumName=InterfaceAIModelEnum"
        );
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .header("Accept", "application/json, text/plain, */*")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(AppError::Upstream(format!(
                "enumDataList failed: status {}",
                resp.status()
            )));
        }

        let envelope: Ouyi7Envelope<Vec<Ouyi7EnumItem>> = resp.json().await?;
        if envelope.code != 200 {
            return Err(AppError::Upstream(format!(
                "enumDataList code: {}",
                envelope.code
            )));
        }

        envelope
            .result
            .ok_or_else(|| AppError::Upstream("enumDataList result is null".to_string()))
    }

    /// 创建话题
    async fn create_topic(&self, model: &str, system_message: &str) -> Result<i64, AppError> {
        let url = format!("{}{}{}", self.base_url, self.chatapi_prefix, "/chat/save");
        let req = Ouyi7CreateTopicRequest {
            id: 0,
            role_id: self.get_role_id_for_model(model),
            title: "Chat".to_string(),
            is_lock: false,
            system_message: system_message.to_string(),
            params: "{}".to_string(),
        };

        let resp = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/plain, */*")
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(AppError::Upstream(format!(
                "create_topic failed: status {}",
                resp.status()
            )));
        }

        let envelope: Ouyi7Envelope<Ouyi7TopicResult> = resp.json().await?;
        if envelope.code != 200 {
            return Err(AppError::Upstream(format!(
                "create_topic code: {}",
                envelope.code
            )));
        }

        envelope
            .result
            .map(|r| r.id)
            .ok_or_else(|| AppError::Upstream("create_topic result is null".to_string()))
    }

    /// 删除话题
    async fn delete_topic(&self, topic_id: i64) -> Result<(), AppError> {
        let url = format!(
            "{}{}{}?id={}",
            self.base_url, self.chatapi_prefix, "/chat/delete", topic_id
        );

        let resp = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/plain, */*")
            .json(&serde_json::json!({}))
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::warn!("delete_topic failed: status {}", resp.status());
        }

        Ok(())
    }

    /// 发送消息第一阶段
    async fn send_message_phase1(
        &self,
        topic_id: i64,
        messages: Vec<crate::model::ouyi_v7::Ouyi7HistoryMessage>,
        content: &str,
    ) -> Result<(i64, i64), AppError> {
        let url = format!(
            "{}{}{}",
            self.base_url, self.chatapi_prefix, "/chat/message"
        );
        let req = Ouyi7ChatMessageRequest {
            topic_id,
            messages,
            content: content.to_string(),
            content_files: vec![],
        };

        let resp = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/plain, */*")
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(AppError::Upstream(format!(
                "message phase1 failed: status {}",
                resp.status()
            )));
        }

        let envelope: Ouyi7Envelope<Ouyi7MessagePhase1Result> = resp.json().await?;
        if envelope.code != 200 {
            return Err(AppError::Upstream(format!(
                "message phase1 code: {}",
                envelope.code
            )));
        }

        envelope
            .result
            .map(|r| r.0)
            .ok_or_else(|| AppError::Upstream("message phase1 result is null".to_string()))
    }

    /// 获取消息响应（第二阶段）
    async fn get_message_response(&self, ai_msg_id: i64) -> Result<String, AppError> {
        let url = format!(
            "{}{}{}{}{}",
            self.base_url, self.chatapi_prefix, "/chat/message/", ai_msg_id, "?retryCount=0"
        );

        let resp = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.jwt_token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/plain, */*")
            .json(&serde_json::json!({}))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(AppError::Upstream(format!(
                "message phase2 failed: status {}",
                resp.status()
            )));
        }

        let text = resp.text().await?;
        Ok(text)
    }

    /// 完整聊天流程：save -> message -> message/{id} -> delete
    pub async fn chat_once(
        &self,
        input: &crate::adapter::ouyi_v7_request::Ouyi7ChatInput,
    ) -> Result<String, AppError> {
        let topic_id = self
            .create_topic(&input.model, &input.system_message)
            .await?;

        let result = async {
            let (_, ai_msg_id) = self
                .send_message_phase1(topic_id, input.messages.clone(), &input.content)
                .await?;
            self.get_message_response(ai_msg_id).await
        }
        .await;

        // 清理话题
        if let Err(e) = self.delete_topic(topic_id).await {
            tracing::warn!("Failed to delete topic {}: {}", topic_id, e);
        }

        result
    }

    /// 根据模型获取 role_id（默认为 1）
    fn get_role_id_for_model(&self, _model: &str) -> i64 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_new() {
        let client = Client::new();
        let ouyi_client = Ouyi7HttpClient::new(
            client,
            "https://api.example.com".to_string(),
            "/chatapi".to_string(),
            "test_token".to_string(),
        );
        assert_eq!(ouyi_client.base_url, "https://api.example.com");
        assert_eq!(ouyi_client.chatapi_prefix, "/chatapi");
    }

    #[test]
    fn test_url_construction() {
        let client = Client::new();
        let ouyi_client = Ouyi7HttpClient::new(
            client,
            "https://api-8.rcouyi.com".to_string(),
            "/chatapi".to_string(),
            "token".to_string(),
        );

        // 验证 URL 构造逻辑
        assert_eq!(
            format!(
                "{}{}{}",
                ouyi_client.base_url, ouyi_client.chatapi_prefix, "/auth/memberInfo"
            ),
            "https://api-8.rcouyi.com/chatapi/auth/memberInfo"
        );
        assert_eq!(
            format!(
                "{}{}",
                ouyi_client.base_url, "/api/sysEnum/enumDataList?EnumName=InterfaceAIModelEnum"
            ),
            "https://api-8.rcouyi.com/api/sysEnum/enumDataList?EnumName=InterfaceAIModelEnum"
        );
    }

    #[test]
    fn test_chat_input_struct_fields() {
        let input = crate::adapter::ouyi_v7_request::Ouyi7ChatInput {
            model: "test-model".to_string(),
            system_message: "You are helpful".to_string(),
            messages: vec![crate::model::ouyi_v7::Ouyi7HistoryMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                content_files: vec![],
            }],
            content: "World".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(1024),
        };

        assert_eq!(input.model, "test-model");
        assert_eq!(input.system_message, "You are helpful");
        assert_eq!(input.messages.len(), 1);
        assert_eq!(input.content, "World");
        assert_eq!(input.temperature, Some(0.7));
        assert_eq!(input.max_tokens, Some(1024));
    }

    #[test]
    fn test_history_message_serialization() {
        let msg = crate::model::ouyi_v7::Ouyi7HistoryMessage {
            role: "assistant".to_string(),
            content: "Test response".to_string(),
            content_files: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"assistant\""));
        assert!(json.contains("\"content\":\"Test response\""));
    }
}
