use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use futures::StreamExt;

use crate::config::{ToolRuntimeConfig, ToolRuntimeProvider};
use crate::error::AppError;
use crate::model::anthropic::MessagesRequest;

const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

pub fn validate_config(config: &ToolRuntimeConfig) -> Result<(), AppError> {
    if config.provider != ToolRuntimeProvider::Anthropic {
        return Err(AppError::BadRequest(
            "当前工具运行时仅支持 Anthropic native provider".to_string(),
        ));
    }
    if config.api_key_env.as_deref().unwrap_or("").is_empty() {
        return Err(AppError::Config(
            "tool_runtime.api_key_env 未配置".to_string(),
        ));
    }
    if config.model.as_deref().unwrap_or("").is_empty() {
        return Err(AppError::Config("tool_runtime.model 未配置".to_string()));
    }
    Ok(())
}

pub async fn proxy_messages(
    http_client: reqwest::Client,
    config: ToolRuntimeConfig,
    mut req: MessagesRequest,
) -> Result<Response, AppError> {
    validate_config(&config)?;

    let api_key_env = config.api_key_env.as_deref().unwrap_or_default();
    let api_key = std::env::var(api_key_env).map_err(|_| AppError::Unauthorized)?;
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_ANTHROPIC_BASE_URL)
        .trim_end_matches('/');
    apply_runtime_model(&config, &mut req);

    let upstream = http_client
        .post(format!("{}/v1/messages", base_url))
        .header("x-api-key", api_key)
        .header("anthropic-version", DEFAULT_ANTHROPIC_VERSION)
        .header(CONTENT_TYPE, "application/json")
        .json(&req)
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("工具运行时请求失败: {}", e)))?;

    let status = upstream.status();
    let headers = build_response_headers(upstream.headers());

    if !status.is_success() {
        let body = upstream.text().await.unwrap_or_default();
        return Err(AppError::Upstream(format!(
            "工具运行时上游错误 {}: {}",
            status, body
        )));
    }

    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
        .unwrap_or_else(|| {
            if req.stream.unwrap_or(false) {
                "text/event-stream".to_string()
            } else {
                "application/json".to_string()
            }
        });

    let stream = upstream
        .bytes_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other));

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type.as_str())
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("anthropic-version", DEFAULT_ANTHROPIC_VERSION)
        .body(Body::from_stream(stream))
        .map(|mut response| {
            *response.headers_mut() = headers;
            response
        })
        .map_err(|e| AppError::Internal(e.to_string()))
}

fn apply_runtime_model(config: &ToolRuntimeConfig, req: &mut MessagesRequest) {
    if let Some(model) = &config.model {
        req.model = model.clone();
    }
}

fn build_response_headers(upstream_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(content_type) = upstream_headers.get(CONTENT_TYPE) {
        headers.insert(CONTENT_TYPE, content_type.clone());
    } else {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    }
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("connection", HeaderValue::from_static("keep-alive"));
    headers.insert(
        "anthropic-version",
        HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
    );
    if let Some(request_id) = upstream_headers.get("request-id") {
        headers.insert("request-id", request_id.clone());
    }
    if let Some(request_id) = upstream_headers.get("x-request-id") {
        headers.insert("x-request-id", request_id.clone());
    }
    headers.remove(AUTHORIZATION);
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToolRuntimeMode, ToolRuntimeProvider};
    use crate::model::anthropic::{AnthropicContent, AnthropicMessage, AnthropicTool};

    fn base_config() -> ToolRuntimeConfig {
        ToolRuntimeConfig {
            mode: ToolRuntimeMode::Native,
            provider: ToolRuntimeProvider::Anthropic,
            base_url: Some("https://api.anthropic.com".to_string()),
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            model: Some("claude-sonnet-4-7".to_string()),
            timeout_secs: 120,
        }
    }

    #[test]
    fn test_validate_config_requires_anthropic_provider() {
        let mut config = base_config();
        config.provider = ToolRuntimeProvider::OpenaiCompatible;

        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_validate_config_requires_api_key_env() {
        let mut config = base_config();
        config.api_key_env = None;

        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_validate_config_accepts_complete_native_config() {
        assert!(validate_config(&base_config()).is_ok());
    }

    #[test]
    fn test_apply_runtime_model_keeps_tool_payload() {
        let config = base_config();
        let mut req = MessagesRequest {
            model: "client-model".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("read file".to_string()),
            }],
            system: None,
            max_tokens: 1024,
            stream: Some(true),
            tools: Some(vec![AnthropicTool {
                name: "Read".to_string(),
                description: "Read file".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"file_path": {"type": "string"}},
                    "required": ["file_path"]
                }),
            }]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        };

        apply_runtime_model(&config, &mut req);
        let value = serde_json::to_value(&req).unwrap();

        assert_eq!(value["model"], "claude-sonnet-4-7");
        assert_eq!(value["tools"][0]["name"], "Read");
        assert_eq!(value["messages"][0]["role"], "user");
        assert!(value.get("temperature").is_none());
        assert!(value.get("thinking").is_none());
    }
}
