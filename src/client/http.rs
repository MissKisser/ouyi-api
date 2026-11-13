//! ouyi 8.0 上游 HTTP 客户端：会话生命周期管理（创建/删除）、模板获取、
//! 聊天流式请求与凭据校验；所有请求自动附带 JWT 鉴权头与客户端版本头。

use crate::adapter::request::OuyiRequest;
use crate::error::AppError;
use crate::model::ouyi::{
    ChatRequest, CreateSessionRequest, CreateSessionResponse, SessionData, TmplResponse,
};

/// ouyi 8.0 上游的 HTTP 客户端，封装鉴权头与端点拼接。
#[derive(Clone)]
pub struct OuyiHttpClient {
    http_client: reqwest::Client,
    base_url: String,
    jwt_token: String,
    app_version: String,
}

impl OuyiHttpClient {
    /// 构建客户端。
    ///
    /// * `http_client` - 底层共享的 reqwest 客户端（代理/超时由外部配置）
    /// * `base_url` - 上游服务根地址
    /// * `jwt_token` - 上游 JWT 凭据，作为 `Authorization` 头
    /// * `app_version` - 客户端版本号，作为 `X-APP-VERSION` 头
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

    /// 拉取上游聊天模板配置。
    ///
    /// 副作用：向上游发起 GET 请求。
    /// 返回 `Err(AppError::Upstream)`：请求失败、HTTP 状态非 2xx 或
    /// 响应体解析失败。
    pub async fn fetch_tmpl(&self) -> Result<TmplResponse, AppError> {
        let url = format!("{}/api/chat/tmpl", self.base_url);
        let resp = self
            .add_auth_headers(self.http_client.get(&url))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AppError::Upstream(e.to_string()))?;

        let body = resp
            .text()
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        tracing::debug!(body = %body, "fetch_tmpl 响应原文");

        serde_json::from_str::<TmplResponse>(&body)
            .map_err(|e| AppError::Upstream(format!("tmpl解析失败: {}", e)))
    }

    /// 在上游创建一个绑定指定模型的聊天会话。
    ///
    /// 副作用：向上游发起 POST 请求，并在上游侧创建会话。
    /// * `model` - 会话使用的模型名
    ///
    /// 返回 `Err(AppError::Upstream)`：请求失败、状态非 2xx、响应解析
    /// 失败、业务码非 0 或返回空数据。
    pub async fn create_session(&self, model: &str) -> Result<SessionData, AppError> {
        let url = format!("{}/api/chat/session", self.base_url);
        let body = CreateSessionRequest::new(model);

        let resp = self
            .add_auth_headers(self.http_client.post(&url))
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AppError::Upstream(format!("创建会话失败: {}", e)))?;

        let body = resp
            .text()
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;

        let create_resp: CreateSessionResponse = serde_json::from_str(&body)
            .map_err(|e| AppError::Upstream(format!("会话响应解析失败: {}", e)))?;

        if create_resp.code != 0 {
            return Err(AppError::Upstream(format!(
                "创建会话错误: {}",
                create_resp.msg
            )));
        }

        create_resp
            .data
            .ok_or_else(|| AppError::Upstream("创建会话返回空数据".to_string()))
    }

    /// 删除上游会话。
    ///
    /// 副作用：向上游发起 DELETE 请求，销毁上游侧会话。
    /// * `session_id` - 上游会话 ID
    ///
    /// 返回 `Err(AppError::Upstream)`：请求失败或状态非 2xx。
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

    /// 向指定会话发送一条文本并返回上游的流式（SSE）响应。
    ///
    /// 副作用：向上游发起 POST 请求。
    /// * `session_id` - 上游会话 ID
    /// * `text` - 发送的文本内容
    /// * `thinking` - 是否开启深度思考模式
    ///
    /// 返回 `Err(AppError::Upstream)`：请求失败或状态非 2xx；
    /// 响应流的读取与解析由调用方负责。
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

    /// 接受 `OuyiRequest`，把 system_prompt 拼成 `[System]: ...\n\n{text}`，
    /// 然后走与 [`Self::chat_stream`] 相同的 8.0 `/api/chat/completions`
    /// SSE 流。
    ///
    /// 副作用与错误语义同 [`Self::chat_stream`]。
    pub async fn create_completion(
        &self,
        session_id: &i64,
        ouyi_req: &OuyiRequest,
    ) -> Result<reqwest::Response, AppError> {
        let final_text = match &ouyi_req.system_prompt {
            Some(sys_prompt) => format!("[System]: {}\n\n{}", sys_prompt, ouyi_req.text),
            None => ouyi_req.text.clone(),
        };
        self.chat_stream(*session_id, final_text, ouyi_req.thinking)
            .await
    }

    /// 校验当前 JWT 凭据是否有效：请求用户信息接口并按 2xx 状态判定。
    ///
    /// 副作用：向上游发起 GET 请求。网络异常一律折叠为 `Ok(false)`；
    /// 恒返回 `Ok`，`Err` 仅在本地请求构造失败时出现。
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
