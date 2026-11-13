//! HTTP 路由组装模块。
//!
//! 把模型列表路由与推理路由合并为应用路由：推理路由（OpenAI chat/completions、
//! Responses、Anthropic messages）统一挂载全局并发限流中间件与工具运行时递归防护中间件；
//! 模型列表路由不受限流约束。管理后台路由与静态资源路由由 `main` 在此基础上合并。

use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::middleware;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::AppState;
use crate::concurrency::limit_inference_requests;
use crate::handler;

/// 组装应用路由。
/// - 模型列表：`/v1/models`、`/openai/v1/models`、`/anthropic/v1/models`；
/// - 推理：`/v1/chat/completions`、`/openai/v1/chat/completions`、`/openai/v1/responses`、
///   `/v1/responses`、`/anthropic/v1/messages`、`/anthropic/messages`，经
///   [`limit_inference_requests`] 限流与 [`reject_tool_runtime_recursion`] 递归防护；
/// - 上述全部路由再经 [`crate::api_auth::require_api_key`] 鉴权（最外层，
///   `[api_keys]` 未配置时放行）。
/// - `state`: 应用共享状态（并发限流器自其中获取）。
///
/// 返回以 `Arc<AppState>` 为状态的 `Router`，由 `main` 再合并管理后台与静态资源路由。
pub fn app(state: Arc<AppState>) -> Router<Arc<AppState>> {
    let limiter = state.concurrency_limiter.clone();
    let models = Router::new()
        .route("/v1/models", get(handler::models::list_models))
        .route("/openai/v1/models", get(handler::models::list_models))
        .route("/anthropic/v1/models", get(handler::anthropic::list_models));

    let inference = Router::new()
        .route(
            "/v1/chat/completions",
            post(handler::chat::chat_completions),
        )
        .route(
            "/openai/v1/chat/completions",
            post(handler::chat::chat_completions),
        )
        .route("/openai/v1/responses", post(handler::responses::responses))
        .route("/v1/responses", post(handler::responses::responses))
        .route("/anthropic/v1/messages", post(handler::anthropic::messages))
        .route("/anthropic/messages", post(handler::anthropic::messages))
        .route_layer(middleware::from_fn_with_state(
            limiter,
            limit_inference_requests,
        ))
        .route_layer(middleware::from_fn(reject_tool_runtime_recursion));

    models
        .merge(inference)
        .route_layer(middleware::from_fn_with_state(
            state,
            crate::api_auth::require_api_key,
        ))
}

/// 工具运行时递归防护中间件：请求携带的递归跳数头部大于等于 1 时，说明是
/// 工具运行时的内部请求回流到了推理入口，直接以 [`AppError::ToolRecursion`]（502）拒绝；
/// 否则放行到下一层处理。
async fn reject_tool_runtime_recursion(request: Request, next: Next) -> Response {
    let hop = request
        .headers()
        .get(crate::tool_runtime::provider::TOOL_RUNTIME_HOP_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u32>().ok());
    if hop.is_some_and(|hop| hop >= 1) {
        return crate::error::AppError::ToolRecursion("内部工具编译请求回到了推理入口".to_string())
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::AccountStore;
    use crate::admin::auth::AdminStore;
    use crate::admin::stats::StatsCollector;
    use crate::client::OuyiClient;
    use crate::client::http::OuyiHttpClient;
    use crate::client::ouyi_v7::Ouyi7HttpClient;
    use crate::config::{
        AdminConfig, ApiKeysConfig, AppConfig, LimitsConfig, Ouyi7Config, OuyiConfig,
        PlatformConfig, ServerConfig, ToolCallConfig, ToolRuntimeConfig, ToolRuntimeMode,
        ToolRuntimeProvider,
    };
    use crate::model::mapper::ModelMapper;
    use crate::platform::{OuyiPlatform, PlatformStatus};
    use axum::body::Body;
    use axum::http::{StatusCode, header};
    use axum::routing::{delete, post};
    use dashmap::DashMap;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::RwLock;

    async fn spawn_http_server(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}", addr)
    }

    async fn spawn_protocol_app(state: Arc<AppState>) -> String {
        let app = super::app(state.clone())
            .merge(crate::admin::admin_routes(state.clone()))
            .with_state(state);
        spawn_http_server(app).await
    }

    fn tool_runtime_config(tool_base_url: String) -> ToolRuntimeConfig {
        ToolRuntimeConfig {
            enabled: true,
            mode: ToolRuntimeMode::Compiler,
            provider: ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some(tool_base_url),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            model: Some("mock-tool-model".to_string()),
            timeout_secs: 5,
        }
    }

    fn make_state(
        platform: OuyiPlatform,
        ouyi_base_url: String,
        tool_base_url: String,
    ) -> Arc<AppState> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();

        let config = AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
            },
            ouyi: OuyiConfig {
                base_url: ouyi_base_url.clone(),
                jwt_token: "test-jwt".to_string(),
                app_version: "3.0.3".to_string(),
                model_refresh_interval_secs: 3600,
            },
            limits: LimitsConfig {
                request_timeout_secs: 5,
                no_data_timeout_secs: 5,
                max_response_bytes: 1024 * 1024,
                max_messages_bytes: 1024 * 1024,
                max_concurrent_requests: 0,
            },
            admin: Some(AdminConfig {
                jwt_secret: Some("test-secret".to_string()),
                password_hash: None,
            }),
            tool_call: ToolCallConfig::default(),
            tool_runtime: Some(tool_runtime_config(tool_base_url.clone())),
            openai_tool_runtime: Some(tool_runtime_config(tool_base_url)),
            platform: Some(PlatformConfig {
                default: platform,
                prompt_on_start: false,
            }),
            ouyi_v7: Some(Ouyi7Config {
                base_url: ouyi_base_url.clone(),
                chatapi_prefix: "/chatapi".to_string(),
                jwt_token: "test-v7-jwt".to_string(),
                default_model: "ouyi-chat".to_string(),
                model_refresh_interval_secs: 3600,
                max_history_messages: 20,
                simulated_stream_chunk_chars: 80,
            }),
            api_keys: None,
        };

        let ouyi_client = match platform {
            OuyiPlatform::Ouyi7 => OuyiClient::V7(Ouyi7HttpClient::new(
                http_client.clone(),
                ouyi_base_url.clone(),
                "/chatapi".to_string(),
                "test-v7-jwt".to_string(),
            )),
            OuyiPlatform::Ouyi8 => OuyiClient::V8(OuyiHttpClient::new(
                http_client.clone(),
                ouyi_base_url.clone(),
                "test-jwt".to_string(),
                "3.0.3".to_string(),
            )),
        };

        Arc::new(AppState {
            concurrency_limiter: crate::concurrency::ConcurrencyLimiter::new(0),
            config: RwLock::new(config),
            http_client,
            ouyi_client: RwLock::new(ouyi_client),
            active_sessions: DashMap::new(),
            tool_sessions: DashMap::new(),
            tool_ledger: crate::tool_runtime::ToolLedger::new(),
            responses_state: crate::responses_state::ResponsesState::default(),
            model_mapper: mapper,
            admin_store: RwLock::new(AdminStore::new("test-secret".to_string(), String::new())),
            account_store: AccountStore::new(),
            stats: StatsCollector::new(),
            platform: RwLock::new(platform),
            platform_status: RwLock::new(PlatformStatus::new(platform, ouyi_base_url)),
        })
    }

    async fn v8_session_handler() -> axum::Json<Value> {
        axum::Json(json!({
            "code": 0,
            "msg": "ok",
            "data": {
                "id": 9001,
                "model": "mock-model",
                "contextCount": 0,
                "temperature": 0.0
            }
        }))
    }

    async fn v8_delete_session_handler() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    async fn v8_chat_handler() -> axum::response::Response {
        let body = format!(
            "data: {{\"id\":\"1\",\"type\":\"string\",\"data\":{}}}\n\n\
             data: {{\"id\":\"1\",\"type\":\"object\",\"data\":{{\"promptTokens\":7,\"completionTokens\":5}}}}\n\n\
             data: {{\"id\":\"1\",\"type\":\"done\"}}\n\n",
            serde_json::to_string("需要查看当前目录。").unwrap()
        );
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }

    fn v8_ouyi_mock_router() -> Router {
        Router::new()
            .route("/api/chat/session", post(v8_session_handler))
            .route("/api/chat/session/{id}", delete(v8_delete_session_handler))
            .route("/api/chat/completions", post(v8_chat_handler))
    }

    async fn tool_compiler_handler(
        axum::extract::State(counter): axum::extract::State<Arc<AtomicUsize>>,
    ) -> axum::Json<Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_mock_bash",
                        "type": "function",
                        "function": {
                            "name": "Bash",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 3
            }
        }))
    }

    fn tool_runtime_mock_router(counter: Arc<AtomicUsize>) -> Router {
        Router::new()
            .route("/chat/completions", post(tool_compiler_handler))
            .with_state(counter)
    }

    fn v7_ouyi_mock_router(reply: &'static str) -> Router {
        async fn save_handler() -> axum::Json<Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": {
                    "id": 12345,
                    "title": "Chat"
                }
            }))
        }

        async fn message_handler() -> axum::Json<Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": [12345, 67890]
            }))
        }

        async fn message_response_handler(
            axum::extract::State(reply): axum::extract::State<&'static str>,
        ) -> axum::response::Response {
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(reply))
                .unwrap()
        }

        async fn delete_handler() -> axum::Json<Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": true
            }))
        }

        Router::new()
            .route("/chatapi/chat/save", post(save_handler))
            .route("/chatapi/chat/message", post(message_handler))
            .route("/chatapi/chat/message/{id}", post(message_response_handler))
            .route("/chatapi/chat/delete", post(delete_handler))
            .with_state(reply)
    }

    fn openai_tool_body() -> Value {
        json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "请查看当前目录"}],
            "stream": false,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "Bash",
                    "description": "Run a shell command",
                    "parameters": {
                        "type": "object",
                        "properties": {"command": {"type": "string"}},
                        "required": ["command"]
                    }
                }
            }],
            "tool_choice": "auto"
        })
    }

    fn openai_text_body() -> Value {
        json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        })
    }

    fn responses_text_body() -> Value {
        json!({
            "model": "gpt-test",
            "input": "hello",
            "stream": false
        })
    }

    fn responses_tools_body() -> Value {
        json!({
            "model": "gpt-test",
            "input": "hello",
            "stream": false,
            "tools": [{
                "type": "function",
                "name": "Bash",
                "description": "Run a shell command",
                "parameters": {"type": "object"}
            }]
        })
    }

    fn anthropic_text_body() -> Value {
        json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        })
    }

    fn anthropic_tool_body() -> Value {
        json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "stream": false,
            "messages": [{"role": "user", "content": "请查看当前目录"}],
            "tools": [{
                "name": "Bash",
                "description": "Run a shell command",
                "input_schema": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }
            }]
        })
    }

    async fn get_json(client: &reqwest::Client, base_url: &str, path: &str) -> (u16, Value) {
        let resp = client
            .get(format!("{base_url}{path}"))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap();
        let value = serde_json::from_str(&body).unwrap_or_else(|_| json!({ "raw": body }));
        (status, value)
    }

    async fn post_json(
        client: &reqwest::Client,
        base_url: &str,
        path: &str,
        body: Value,
    ) -> (u16, Value) {
        let resp = client
            .post(format!("{base_url}{path}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap();
        let value = serde_json::from_str(&body).unwrap_or_else(|_| json!({ "raw": body }));
        (status, value)
    }

    #[tokio::test]
    async fn production_inference_routes_share_limiter_but_model_routes_do_not() {
        let state = make_state(
            OuyiPlatform::Ouyi8,
            "http://127.0.0.1:9".to_string(),
            "http://127.0.0.1:9".to_string(),
        );
        state.concurrency_limiter.set_limit(1);
        let _permit = state
            .concurrency_limiter
            .try_acquire()
            .expect("the only permit should be available");
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        for path in [
            "/v1/chat/completions",
            "/openai/v1/responses",
            "/v1/responses",
            "/anthropic/v1/messages",
            "/anthropic/messages",
        ] {
            let response = client
                .post(format!("{base_url}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{path}");
            assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "1");

            let body: Value = response.json().await.unwrap();
            assert_eq!(
                body["error"]["message"], "Global concurrency limit reached",
                "{path}"
            );
            if path.starts_with("/anthropic") {
                assert_eq!(body["type"], "error", "{path}");
                assert_eq!(body["error"]["type"], "rate_limit_error", "{path}");
            } else {
                assert_eq!(body["error"]["type"], "rate_limit_error", "{path}");
                assert_eq!(
                    body["error"]["code"], "concurrency_limit_exceeded",
                    "{path}"
                );
            }
        }

        for path in ["/v1/models", "/openai/v1/models", "/anthropic/v1/models"] {
            let response = client
                .get(format!("{base_url}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }

    #[tokio::test]
    async fn tool_runtime_hop_is_rejected_before_upstream_for_all_inference_routes() {
        let state = make_state(
            OuyiPlatform::Ouyi8,
            "http://127.0.0.1:9".to_string(),
            "http://127.0.0.1:9".to_string(),
        );
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        for (path, body) in [
            ("/v1/chat/completions", openai_text_body()),
            ("/openai/v1/responses", responses_text_body()),
            ("/v1/responses", responses_text_body()),
            ("/anthropic/v1/messages", anthropic_text_body()),
            ("/anthropic/messages", anthropic_text_body()),
        ] {
            let response = client
                .post(format!("{base_url}{path}"))
                .header(crate::tool_runtime::provider::TOOL_RUNTIME_HOP_HEADER, "1")
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "{path}");
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["error"]["type"], "tool_runtime_recursion", "{path}");
        }
    }

    #[tokio::test]
    async fn protocol_routes_v8_smoke_with_mocked_ouyi_and_tool_runtime() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_http_server(v8_ouyi_mock_router()).await;
        let tool_base_url =
            spawn_http_server(tool_runtime_mock_router(tool_requests.clone())).await;
        let state = make_state(OuyiPlatform::Ouyi8, ouyi_base_url, tool_base_url);
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::new();

        for path in ["/v1/models", "/openai/v1/models", "/anthropic/v1/models"] {
            let (status, value) = get_json(&client, &base_url, path).await;
            assert_eq!(status, 200, "{path}");
            assert!(
                value["data"]
                    .as_array()
                    .is_some_and(|models| !models.is_empty())
            );
        }

        let (status, value) = post_json(
            &client,
            &base_url,
            "/v1/chat/completions",
            openai_tool_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "Bash"
        );

        let (status, value) = post_json(
            &client,
            &base_url,
            "/openai/v1/responses",
            responses_text_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            value["output"][0]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("当前目录"))
        );

        let (status, value) = post_json(
            &client,
            &base_url,
            "/openai/v1/responses",
            responses_tools_body(),
        )
        .await;
        assert_eq!(status, 200);
        // 验证 tool_call 输出
        let output = value["output"].as_array().expect("output 应该是数组");
        assert!(!output.is_empty(), "output 应至少含一个项");
        assert_eq!(output[0]["type"], "function_call");
        assert!(output[0]["call_id"].is_string());
        assert!(output[0]["name"].is_string());
        assert!(output[0]["arguments"].is_string());

        for path in ["/anthropic/v1/messages", "/anthropic/messages"] {
            let (status, value) = post_json(&client, &base_url, path, anthropic_tool_body()).await;
            assert_eq!(status, 200, "{path}");
            assert_eq!(value["stop_reason"], "tool_use");
            assert_eq!(value["content"][0]["type"], "tool_use");
            assert_eq!(value["content"][0]["name"], "Bash");
        }

        let (status, _) = get_json(&client, &base_url, "/admin/api/status").await;
        assert_eq!(status, 401);

        // 流式 + tools
        let mut stream_body = responses_tools_body();
        stream_body["stream"] = json!(true);
        let resp = client
            .post(format!("{base_url}/openai/v1/responses"))
            .json(&stream_body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        // 验证关键事件存在
        assert!(
            body.contains("response.created"),
            "应有 response.created 事件"
        );
        assert!(
            body.contains("response.output_item.added"),
            "应有 output_item.added 事件"
        );
        assert!(
            body.contains("response.function_call_arguments"),
            "应有 function_call_arguments 事件"
        );
        assert!(
            body.contains("response.completed"),
            "应有 response.completed 事件"
        );

        assert_eq!(tool_requests.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn protocol_routes_v7_text_smoke_with_mocked_ouyi() {
        let ouyi_base_url = spawn_http_server(v7_ouyi_mock_router("这是 7.0 mock 文本。")).await;
        let tool_base_url =
            spawn_http_server(tool_runtime_mock_router(Arc::new(AtomicUsize::new(0)))).await;
        let state = make_state(OuyiPlatform::Ouyi7, ouyi_base_url, tool_base_url);
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::new();

        let (status, value) = post_json(
            &client,
            &base_url,
            "/v1/chat/completions",
            openai_text_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            value["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|text| text.contains("7.0 mock"))
        );

        let (status, value) = post_json(
            &client,
            &base_url,
            "/openai/v1/responses",
            responses_text_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            value["output"][0]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("7.0 mock"))
        );

        let (status, value) = post_json(
            &client,
            &base_url,
            "/anthropic/v1/messages",
            anthropic_text_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(value["stop_reason"], "end_turn");
        assert!(
            value["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("7.0 mock"))
        );

        let (status, value) = post_json(
            &client,
            &base_url,
            "/openai/v1/responses",
            responses_tools_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(value["output"][0]["type"], "function_call");
    }

    #[tokio::test]
    async fn protocol_routes_v7_tools_smoke_with_mocked_ouyi() {
        let tool_reply = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );
        let ouyi_base_url = spawn_http_server(v7_ouyi_mock_router(tool_reply)).await;
        let tool_base_url =
            spawn_http_server(tool_runtime_mock_router(Arc::new(AtomicUsize::new(0)))).await;
        let state = make_state(OuyiPlatform::Ouyi7, ouyi_base_url, tool_base_url);
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::new();

        let (status, value) = post_json(
            &client,
            &base_url,
            "/v1/chat/completions",
            openai_tool_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "Bash"
        );

        let (status, value) = post_json(
            &client,
            &base_url,
            "/anthropic/messages",
            anthropic_tool_body(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["content"][0]["type"], "tool_use");
        assert_eq!(value["content"][0]["name"], "Bash");
    }

    async fn enable_api_keys(state: &Arc<AppState>, keys: &[&str]) {
        state.config.write().await.api_keys = Some(ApiKeysConfig {
            keys: keys.iter().map(|k| k.to_string()).collect(),
        });
    }

    #[tokio::test]
    async fn inference_open_when_api_keys_not_configured() {
        let state = make_state(
            OuyiPlatform::Ouyi8,
            "http://127.0.0.1:1".to_string(),
            String::new(),
        );
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        for headers in [None, Some(("authorization", "Bearer sk-anything-goes"))] {
            let mut req = client.get(format!("{base_url}/v1/models"));
            if let Some((name, value)) = headers {
                req = req.header(name, value);
            }
            let response = req.send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn inference_rejects_missing_or_wrong_key_when_enabled() {
        let state = make_state(
            OuyiPlatform::Ouyi8,
            "http://127.0.0.1:1".to_string(),
            String::new(),
        );
        enable_api_keys(&state, &["sk-correct-key-0123456789"]).await;
        let base_url = spawn_protocol_app(state.clone()).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        let missing = client
            .get(format!("{base_url}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        let body: serde_json::Value = missing.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_api_key");

        let wrong = client
            .get(format!("{base_url}/v1/models"))
            .header("authorization", "Bearer sk-wrong-key-0123456789")
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let anthropic_missing = client
            .post(format!("{base_url}/anthropic/v1/messages"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(anthropic_missing.status(), StatusCode::UNAUTHORIZED);
        let a_body: serde_json::Value = anthropic_missing.json().await.unwrap();
        assert_eq!(a_body["error"]["type"], "authentication_error");
    }

    #[tokio::test]
    async fn inference_accepts_valid_key_and_admin_jwt_when_enabled() {
        let state = make_state(
            OuyiPlatform::Ouyi8,
            "http://127.0.0.1:1".to_string(),
            String::new(),
        );
        enable_api_keys(&state, &["sk-correct-key-0123456789"]).await;
        let admin_token = state
            .admin_store
            .read()
            .await
            .generate_token("admin")
            .unwrap();
        let base_url = spawn_protocol_app(state).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        for header in [
            ("authorization", "Bearer sk-correct-key-0123456789"),
            ("x-api-key", "sk-correct-key-0123456789"),
            ("authorization", &format!("Bearer {admin_token}")),
        ] {
            let response = client
                .get(format!("{base_url}/v1/models"))
                .header(header.0, header.1)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{header:?}");
        }
    }

    #[tokio::test]
    async fn admin_login_unaffected_by_api_key_auth() {
        let state = make_state(
            OuyiPlatform::Ouyi8,
            "http://127.0.0.1:1".to_string(),
            String::new(),
        );
        enable_api_keys(&state, &["sk-correct-key-0123456789"]).await;
        let base_url = spawn_protocol_app(state).await;

        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{base_url}/admin/api/login"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        let text = response.text().await.unwrap();
        assert!(!text.contains("invalid_api_key"));
    }
}
