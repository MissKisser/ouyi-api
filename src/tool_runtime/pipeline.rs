//! 工具调用决策 + ledger + stats 流水线的统一入口。
//! 供 `handler::chat` 与 `handler::responses` 共用。

use std::time::Instant;

use crate::AppState;
use crate::model::openai::ChatCompletionRequest;
use crate::tool_runtime::openai_compiler::CompiledToolCall;

/// 工具流水线的可观测元数据。
/// 设计为公共 API 表面，调用方（聊天 / 响应处理器）可在响应体或日志中透出。
/// 当前仅 `prompt_tokens` / `completion_tokens` 被消费，其它字段预留供后续：
/// - `provider` / `model` / `base_url`：工具模型来源（用于调试日志与统计页）
/// - `platform_key` / `short_model`：客户端 → Ouyi 模型的映射
#[derive(Debug, Clone)]
pub struct PipelineMeta {
    #[allow(dead_code)] // reserved for telemetry/debug
    pub provider: String,
    #[allow(dead_code)] // reserved for telemetry/debug
    pub model: String,
    #[allow(dead_code)] // reserved for telemetry/debug
    pub base_url: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[allow(dead_code)] // reserved for telemetry/debug
    pub platform_key: String,
    #[allow(dead_code)] // reserved for telemetry/debug
    pub short_model: String,
}

impl PipelineMeta {
    /// 从决策层 meta 构造 pipeline meta，注入调用方提供的 platform_key / short_model。
    fn from_decision(
        dmeta: &crate::tool_runtime::decision::ToolDecisionMeta,
        platform_key: &str,
        short_model: &str,
    ) -> Self {
        Self {
            provider: dmeta.provider.clone(),
            model: dmeta.model.clone(),
            base_url: if dmeta.base_url.is_empty() {
                None
            } else {
                Some(dmeta.base_url.clone())
            },
            prompt_tokens: dmeta.usage.prompt_tokens,
            completion_tokens: dmeta.usage.completion_tokens,
            platform_key: platform_key.to_string(),
            short_model: short_model.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PipelineOutput {
    ToolCalls {
        calls: Vec<CompiledToolCall>,
        meta: PipelineMeta,
    },
    FinalAnswer {
        text: String,
        meta: PipelineMeta,
    },
}

pub async fn run_openai_tool_pipeline(
    state: &AppState,
    request: &ChatCompletionRequest,
    upstream_output: String,
    platform_key: &str,
    start_time: Instant,
) -> PipelineOutput {
    use crate::tool_runtime::decision::{
        ToolDecisionMode, ToolTextDecision, decide_openai_tools_from_text,
    };

    let short_model = request.model.clone();
    let runtime_cfg = state.config.read().await.openai_tool_runtime.clone();

    // 阶段 A：决策
    let decision = match decide_openai_tools_from_text(
        state.http_client.clone(),
        runtime_cfg.as_ref(),
        request,
        &upstream_output,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "openai tool pipeline: decision failed");
            return PipelineOutput::FinalAnswer {
                text: upstream_output,
                meta: PipelineMeta {
                    provider: "error".into(),
                    model: "error".into(),
                    base_url: None,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    platform_key: platform_key.into(),
                    short_model: short_model.clone(),
                },
            };
        }
    };

    match decision {
        ToolTextDecision::ToolCalls { calls, meta: dmeta } => {
            // 阶段 B：ledger 写入
            for call in &calls {
                let call_key = format!("{}:{}", short_model, call.id);
                if let Err(e) = state
                    .tool_ledger
                    .record_tool_use(&call_key, &call.id, &call.name)
                {
                    tracing::warn!(error = %e, call_id = %call.id, "ledger.record_tool_use failed; degrading to FinalAnswer");
                    return PipelineOutput::FinalAnswer {
                        text: upstream_output,
                        meta: PipelineMeta::from_decision(&dmeta, platform_key, &short_model),
                    };
                }
                let broker_call = crate::tool_runtime::broker::BrokerToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                };
                if let Some(plan) = crate::tool_runtime::broker::detect_long_task(&broker_call) {
                    if let Err(e) = state.tool_ledger.attach_long_task(
                        &call_key,
                        plan.file_path,
                        plan.done_marker,
                        plan.max_polls,
                    ) {
                        tracing::warn!(error = %e, "ledger.attach_long_task failed; continuing");
                    }
                }
            }
            // 阶段 D：记录 stats（tool_runtime 自身）
            if dmeta.mode != ToolDecisionMode::None {
                use crate::admin::stats::{ToolRuntimeDecision, ToolRuntimeMode as StatsToolRuntimeMode};
                let mode = match dmeta.mode {
                    ToolDecisionMode::Broker => StatsToolRuntimeMode::Broker,
                    ToolDecisionMode::Compiler => StatsToolRuntimeMode::Compiler,
                    ToolDecisionMode::None => unreachable!(),
                };
                let tool_names: Vec<String> = calls.iter().map(|c| c.name.clone()).collect();
                let runtime_started = Instant::now();
                state.stats.record_tool_runtime_success(
                    "",
                    "openai",
                    mode,
                    &dmeta.provider,
                    &dmeta.model,
                    &dmeta.base_url,
                    ToolRuntimeDecision::ToolCalls,
                    tool_names,
                    dmeta.usage.prompt_tokens,
                    dmeta.usage.completion_tokens,
                    runtime_started.elapsed().as_millis() as u64,
                );
            }
            tracing::info!(
                provider = %dmeta.provider,
                model = %dmeta.model,
                base_url = %dmeta.base_url,
                platform_key = platform_key,
                short_model = %short_model,
                call_count = calls.len(),
                "openai tool pipeline: tool calls"
            );
            PipelineOutput::ToolCalls {
                calls,
                meta: PipelineMeta::from_decision(&dmeta, platform_key, &short_model),
            }
        }
        ToolTextDecision::FinalAnswer { meta: dmeta } => {
            tracing::info!(
                provider = %dmeta.provider,
                model = %dmeta.model,
                base_url = %dmeta.base_url,
                platform_key = platform_key,
                short_model = %short_model,
                "openai tool pipeline: final answer"
            );
            // 复刻 chat 路径原行为：Compiler 模式下额外记一次 tool_runtime stats
            if dmeta.mode == ToolDecisionMode::Compiler {
                use crate::admin::stats::{ToolRuntimeDecision, ToolRuntimeMode as StatsToolRuntimeMode};
                let runtime_started = Instant::now();
                state.stats.record_tool_runtime_success(
                    "",
                    "openai",
                    StatsToolRuntimeMode::Compiler,
                    &dmeta.provider,
                    &dmeta.model,
                    &dmeta.base_url,
                    ToolRuntimeDecision::FinalAnswer,
                    Vec::new(),
                    dmeta.usage.prompt_tokens,
                    dmeta.usage.completion_tokens,
                    runtime_started.elapsed().as_millis() as u64,
                );
            }
            state.stats.record_success(
                start_time.elapsed().as_millis() as u64,
                dmeta.usage.prompt_tokens,
                dmeta.usage.completion_tokens,
                &short_model,
                platform_key,
            );
            PipelineOutput::FinalAnswer {
                text: upstream_output,
                meta: PipelineMeta::from_decision(&dmeta, platform_key, &short_model),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppConfig, LimitsConfig, Ouyi7Config, OuyiConfig, ServerConfig, ToolCallConfig,
        ToolRuntimeConfig, ToolRuntimeMode, ToolRuntimeProvider,
    };
    use crate::platform::{OuyiPlatform, PlatformStatus};
    use crate::tool_runtime::ToolLedger;
    use crate::admin::auth::AdminStore;
    use crate::admin::stats::StatsCollector;
    use crate::model::mapper::ModelMapper;
    use dashmap::DashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock as AsyncRwLock;

    #[test]
    fn pipeline_meta_carries_compiler_usage_and_platform_key() {
        let meta = PipelineMeta {
            provider: "openai-compatible".to_string(),
            model: "gpt-tool".to_string(),
            base_url: Some("http://tool.local/v1".to_string()),
            prompt_tokens: 11,
            completion_tokens: 3,
            platform_key: "ouyi8".to_string(),
            short_model: "gpt-test".to_string(),
        };
        assert_eq!(meta.prompt_tokens, 11);
        assert_eq!(meta.completion_tokens, 3);
        assert_eq!(meta.platform_key, "ouyi8");
    }

    #[test]
    fn pipeline_output_tool_calls_carries_calls_and_meta() {
        let calls = vec![CompiledToolCall {
            id: "call_1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }];
        let out = PipelineOutput::ToolCalls {
            calls: calls.clone(),
            meta: PipelineMeta {
                provider: "x".into(),
                model: "y".into(),
                base_url: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                platform_key: "ouyi8".into(),
                short_model: "gpt-test".into(),
            },
        };
        match out {
            PipelineOutput::ToolCalls { calls, .. } => {
                assert_eq!(calls[0].name, "Bash");
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    /// 构造最小可用 AppState 用于 pipeline 单元测试。
    /// 不依赖真实 OUYI，决策阶段只读 config.openai_tool_runtime。
    fn test_state(runtime_cfg: Option<ToolRuntimeConfig>) -> Arc<AppState> {
        let http_client = reqwest::Client::new();
        let config = AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
            },
            ouyi: OuyiConfig {
                base_url: "http://localhost".into(),
                jwt_token: "t".into(),
                app_version: "3.0.3".into(),
                model_refresh_interval_secs: 3600,
            },
            limits: LimitsConfig {
                request_timeout_secs: 5,
                no_data_timeout_secs: 5,
                max_response_bytes: 1024,
                max_messages_bytes: 1024,
                max_concurrent_requests: 0,
            },
            admin: None,
            tool_call: ToolCallConfig::default(),
            tool_runtime: runtime_cfg.clone(),
            openai_tool_runtime: runtime_cfg,
            platform: None,
            ouyi_v7: Some(Ouyi7Config {
                base_url: "http://localhost".into(),
                chatapi_prefix: "/chatapi".into(),
                jwt_token: "t".into(),
                default_model: "x".into(),
                model_refresh_interval_secs: 3600,
                max_history_messages: 20,
                simulated_stream_chunk_chars: 80,
            }),
        };
        let mapper = ModelMapper::new();
        Arc::new(AppState {
            concurrency_limiter: crate::concurrency::ConcurrencyLimiter::new(0),
            config: AsyncRwLock::new(config),
            http_client: http_client.clone(),
            ouyi_client: AsyncRwLock::new(crate::client::OuyiClient::V8(
                crate::client::http::OuyiHttpClient::new(
                    http_client.clone(),
                    "http://localhost".into(),
                    "t".into(),
                    "3.0.3".into(),
                ),
            )),
            active_sessions: DashMap::new(),
            tool_sessions: DashMap::new(),
            tool_ledger: ToolLedger::new(),
            model_mapper: mapper,
            admin_store: AsyncRwLock::new(AdminStore::new("s".into(), String::new())),
            account_store: crate::admin::AccountStore::new(),
            stats: StatsCollector::new(),
            platform: AsyncRwLock::new(OuyiPlatform::Ouyi8),
            platform_status: AsyncRwLock::new(PlatformStatus::new(
                OuyiPlatform::Ouyi8,
                "http://localhost".into(),
            )),
        })
    }

    fn disabled_cfg() -> ToolRuntimeConfig {
        ToolRuntimeConfig {
            enabled: false,
            mode: ToolRuntimeMode::Compiler,
            provider: ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some("http://tool.local/v1".into()),
            api_key: Some("sk-test".into()),
            api_key_env: None,
            model: Some("mock".into()),
            timeout_secs: 5,
        }
    }

    fn chat_req_with_tools() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-test".into(),
            messages: vec![crate::model::openai::ChatMessage {
                role: "user".into(),
                content: Some("请执行 ls".into()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: Some(vec![crate::model::openai::ToolDef {
                tool_type: "function".into(),
                function: crate::model::openai::ToolFunction {
                    name: "Bash".into(),
                    description: Some("Run shell".into()),
                    parameters: Some(serde_json::json!({"type": "object"})),
                },
            }]),
            tool_choice: None,
            parallel_tool_calls: None,
        }
    }

    #[tokio::test]
    async fn pipeline_final_answer_path_records_meta_and_text() {
        // 上游文本不包含 tool markers 且 runtime 禁用 → FinalAnswer
        let state = test_state(Some(disabled_cfg()));
        let req = chat_req_with_tools();
        let out = run_openai_tool_pipeline(
            &state,
            &req,
            "我不调用任何工具".into(),
            "ouyi8",
            Instant::now(),
        )
        .await;
        match out {
            PipelineOutput::FinalAnswer { text, meta } => {
                assert_eq!(text, "我不调用任何工具");
                assert_eq!(meta.platform_key, "ouyi8");
                assert_eq!(meta.short_model, "gpt-test");
            }
            _ => panic!("expected FinalAnswer"),
        }
    }
}
