use crate::error::AppError;
use crate::model::catalog::ModelCatalog;
use crate::model::ouyi_v7::{
    MemberInfoResult, Ouyi7ChatMessageRequest, Ouyi7CreateTopicRequest, Ouyi7EnumItem,
    Ouyi7Envelope, Ouyi7MessagePhase1Result, Ouyi7TopicResult,
};
use dashmap::DashMap;
use reqwest::Client;
use std::sync::Arc;

/// 7.0 模型路由策略。
///
/// 实地 JWT 探测 api-8.rcouyi.com 后端确认的两种路由方式：
/// - `Custom`：customAIModels 字符串名 → `roleId=1` + `params.model = "<name>"`
/// - `Standard`：aiModels 数字 value → `roleId = <value>`
///
/// 后端会忽略其他方式选择模型，默认 fallback 到 gpt-3.5-turbo —— 这是
/// 旧版反代"模型路由完全失效"的根因（params 永远是 "{}"、role_id 恒为 1）。
#[derive(Debug, Clone)]
pub enum V7ModelRouting {
    /// customAIModel，走 roleId=1 + params.model
    Custom { model: String },
    /// 标准模型，走 roleId=value
    Standard { role_id: i64 },
}

impl V7ModelRouting {
    /// 应用到 `Ouyi7CreateTopicRequest` 的 role_id 与 params 字段。
    fn apply(&self) -> (i64, String) {
        match self {
            V7ModelRouting::Custom { model: stored } => {
                // 用表里存的 model（catalog 构造时的精确名），避免大小写差异
                (1, serde_json::to_string(&serde_json::json!({ "model": stored })).unwrap_or_default())
            }
            V7ModelRouting::Standard { role_id } => (*role_id, "{}".to_string()),
        }
    }
}

impl V7ModelRouting {
    pub fn as_custom(model: &str) -> Self {
        V7ModelRouting::Custom { model: model.to_string() }
    }
}

/// 从 ModelCatalog 构造路由表（id → V7ModelRouting）。
///
/// 由 main.rs 的后台刷新任务在每次 catalog 加载后调用，推送给 client。
pub fn build_routing_table_from_catalog(catalog: &ModelCatalog) -> Arc<DashMap<String, V7ModelRouting>> {
    let table: Arc<DashMap<String, V7ModelRouting>> = Arc::new(DashMap::new());
    for item in &catalog.models {
        let Some(meta) = &item.metadata else { continue };
        let routing = meta.get("routing").and_then(|v| v.as_str());
        match routing {
            Some("custom") => {
                if let Some(model) = meta.get("model").and_then(|v| v.as_str()) {
                    table.insert(item.id.clone(), V7ModelRouting::Custom { model: model.to_string() });
                }
            }
            Some("standard") => {
                if let Some(role_id) = meta.get("role_id").and_then(|v| v.as_i64()) {
                    table.insert(item.id.clone(), V7ModelRouting::Standard { role_id });
                }
            }
            _ => {}
        }
    }
    table
}

/// 7.0 HTTP 客户端
#[derive(Clone)]
pub struct Ouyi7HttpClient {
    http_client: Client,
    base_url: String,
    chatapi_prefix: String,
    jwt_token: String,
    /// 模型路由表（id → V7ModelRouting）。由 main.rs 的后台刷新任务推送。
    /// 默认空 —— 空表时 create_topic 走 Fallback（roleId=1 + params.model=model）。
    routing_table: Arc<DashMap<String, V7ModelRouting>>,
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
            routing_table: Arc::new(DashMap::new()),
        }
    }

    /// 用最新 catalog 构造的路由表替换 client 持有的表。
    /// 由 main.rs 后台刷新任务调用；对正在处理中的请求无影响（旧请求已读到旧值）。
    pub fn update_routing(&self, table: Arc<DashMap<String, V7ModelRouting>>) {
        // 用 ptr_eq 避免无谓替换
        if !Arc::ptr_eq(&table, &self.routing_table) {
            // DashMap 没有 swap，用 clear + extend 模拟替换
            self.routing_table.clear();
            for entry in table.iter() {
                self.routing_table.insert(entry.key().clone(), entry.value().clone());
            }
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
                "memberInfo code: {} message: {}",
                envelope.code,
                envelope.message
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
                "enumDataList code: {} message: {}",
                envelope.code,
                envelope.message
            )));
        }

        envelope
            .result
            .ok_or_else(|| AppError::Upstream("enumDataList result is null".to_string()))
    }

    /// 创建话题
    ///
    /// 模型路由由 `routing_table` 决定：
    /// - 命中 `Custom` → `roleId=1` + `params={"model": "<name>"}`
    /// - 命中 `Standard` → `roleId=<value>` + `params="{}"`
    /// - 未命中（表为空或 model 不在表里）→ **Fallback**：按 Custom 路径走
    ///   （`roleId=1` + `params={"model": "<user_input>"}`），让后端尽量识别。
    async fn create_topic(&self, model: &str, system_message: &str) -> Result<i64, AppError> {
        let url = format!("{}{}{}", self.base_url, self.chatapi_prefix, "/chat/save");
        let (role_id, params) = match self.routing_table.get(model) {
            Some(entry) => entry.apply(),
            None => {
                // Fallback：未知模型按 custom 路径（roleId=1 + params.model=model）
                tracing::debug!(
                    model,
                    "V7 routing miss, falling back to roleId=1 + params.model"
                );
                (1_i64, serde_json::to_string(&serde_json::json!({ "model": model })).unwrap_or_default())
            }
        };
        let req = Ouyi7CreateTopicRequest {
            id: 0,
            role_id,
            title: "Chat".to_string(),
            is_lock: false,
            system_message: system_message.to_string(),
            params,
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
                "create_topic code: {} message: {}",
                envelope.code,
                envelope.message
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
                "message phase1 code: {} message: {}",
                envelope.code,
                envelope.message
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

    /// V7ModelRouting::apply —— 两种路由对应的 role_id 和 params
    #[test]
    fn routing_apply_custom_vs_standard() {
        let custom = V7ModelRouting::Custom { model: "deepseek-chat".to_string() };
        let (rid, params) = custom.apply();
        assert_eq!(rid, 1);
        assert!(params.contains("\"model\":\"deepseek-chat\""),
            "Custom 路由应把 model 写进 params，实际: {params}");

        let standard = V7ModelRouting::Standard { role_id: 35 };
        let (rid, params) = standard.apply();
        assert_eq!(rid, 35, "gpt-4o 的 role_id 应为 value 35");
        assert_eq!(params, "{}", "Standard 路由的 params 应为空 JSON");
    }

    /// 路由表构造 + client.update_routing —— 端到端验证 catalog→table→client 流转
    #[test]
    fn routing_table_from_catalog_and_client_update() {
        use crate::model::catalog::{infer_ouyi_v7_capabilities, ModelCatalog, ModelCatalogItem};
        use crate::platform::OuyiPlatform;

        let catalog = ModelCatalog {
            platform: OuyiPlatform::Ouyi7,
            models: vec![
                ModelCatalogItem {
                    id: "deepseek-chat".to_string(),
                    label: "deepseek-chat".to_string(),
                    provider_key: "deepseek".to_string(),
                    provider_name: "DeepSeek".to_string(),
                    capabilities: infer_ouyi_v7_capabilities("deepseek-chat"),
                    metadata: Some(serde_json::json!({"routing": "custom", "model": "deepseek-chat"})),
                },
                ModelCatalogItem {
                    id: "ChatGPT4o".to_string(),
                    label: "GPT-4o".to_string(),
                    provider_key: "openai".to_string(),
                    provider_name: "OpenAI".to_string(),
                    capabilities: infer_ouyi_v7_capabilities("GPT-4o"),
                    metadata: Some(serde_json::json!({"routing": "standard", "role_id": 35})),
                },
            ],
        };

        let table = build_routing_table_from_catalog(&catalog);
        assert_eq!(table.len(), 2);

        // 校验两种路由都被正确解码
        match table.get("deepseek-chat").as_deref() {
            Some(V7ModelRouting::Custom { model }) => assert_eq!(model, "deepseek-chat"),
            other => panic!("deepseek-chat 应是 Custom，实际: {:?}", other.map(|v| format!("{:?}", v))),
        }
        match table.get("ChatGPT4o").as_deref() {
            Some(V7ModelRouting::Standard { role_id }) => assert_eq!(*role_id, 35),
            other => panic!("ChatGPT4o 应是 Standard{{35}}，实际: {:?}", other.map(|v| format!("{:?}", v))),
        }

        // client.update_routing 把表写入 client
        let client = Ouyi7HttpClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            "http://127.0.0.1:1".to_string(),
            "/chatapi".to_string(),
            "test-jwt".to_string(),
        );
        assert_eq!(client.routing_table.len(), 0, "新建 client 应是空表");
        client.update_routing(table);
        assert_eq!(client.routing_table.len(), 2, "update 后应有路由");
        assert!(client.routing_table.get("deepseek-chat").is_some());
    }
}
