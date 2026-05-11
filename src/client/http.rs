use crate::error::AppError;
use crate::model::ouyi::{
    ChatRequest, CreateSessionResponse, SessionData, TmplResponse,
};

#[derive(Clone)]
pub struct OuyiHttpClient {
    http_client: reqwest::Client,
    base_url: String,
    jwt_token: String,
    app_version: String,
}

impl OuyiHttpClient {
    pub fn new(
        http_client: reqwest::Client,
        base_url: String,
        jwt_token: String,
        app_version: String,
    ) -> Self {
        Self {
            http_client,
            base_url,
            jwt_token,
            app_version,
        }
    }

    fn add_auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Authorization", &self.jwt_token)
            .header("X-APP-VERSION", &self.app_version)
    }

    pub async fn fetch_tmpl(&self) -> Result<TmplResponse, AppError> {
        let url = format!("{}/api/chat/tmpl", self.base_url);
        let resp = self
            .add_auth_headers(self.http_client.get(&url))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AppError::Upstream(e.to_string()))?;

        let body = resp.text().await.map_err(|e| AppError::Upstream(e.to_string()))?;

        serde_json::from_str::<TmplResponse>(&body)
            .map_err(|e| AppError::Upstream(format!("tmpl解析失败: {}", e)))
    }

    pub async fn create_session(&self, model: &str) -> Result<SessionData, AppError> {
        let url = format!("{}/api/chat/session", self.base_url);
        let body = serde_json::json!({
            "model": model
        });

        let resp = self
            .add_auth_headers(self.http_client.post(&url))
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AppError::Upstream(format!("创建会话失败: {}", e)))?;

        let body = resp.text().await.map_err(|e| AppError::Upstream(e.to_string()))?;

        let create_resp: CreateSessionResponse = serde_json::from_str(&body)
            .map_err(|e| AppError::Upstream(format!("会话响应解析失败: {}", e)))?;

        if create_resp.code != 0 {
            return Err(AppError::Upstream(format!(
                "创建会话错误: {}",
                create_resp.msg
            )));
        }

        create_resp.data.ok_or_else(|| AppError::Upstream("创建会话返回空数据".to_string()))
    }

    pub async fn delete_session(&self, session_id: i64) -> Result<(), AppError> {
        let url = format!("{}/api/chat/session/{}", self.base_url, session_id);
        self.add_auth_headers(self.http_client.delete(&url))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AppError::Upstream(format!("删除会话失败: {}", e)))?;
        tracing::debug!(session_id, "会话已清理");
        Ok(())
    }

    pub async fn chat_stream(
        &self,
        session_id: i64,
        text: String,
        thinking: bool,
    ) -> Result<reqwest::Response, AppError> {
        let url = format!("{}/api/chat/completions", self.base_url);
        let chat_req = ChatRequest {
            text,
            session_id,
            files: vec![],
            thinking,
        };

        let resp = self
            .add_auth_headers(self.http_client.post(&url))
            .json(&chat_req)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AppError::Upstream(format!("聊天请求失败: {}", e)))?;

        Ok(resp)
    }

    pub async fn verify_token(&self) -> Result<bool, AppError> {
        let url = format!("{}/api/user/info", self.base_url);
        let resp = self
            .add_auth_headers(self.http_client.get(&url))
            .send()
            .await;

        match resp {
            Ok(r) => Ok(r.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}
