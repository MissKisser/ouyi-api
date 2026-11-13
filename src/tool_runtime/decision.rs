//! OpenAI 协议的工具决策入口：根据主模型的文本输出决定调用哪些工具。
//! 先走本地 broker 解析（标记 / 中文规则），未命中且配置启用时再调用外部
//! 编译器；产出带决策来源与 token 用量元数据的决策结果，供 OpenAI 响应
//! 处理器组装后续消息。

use crate::config::ToolRuntimeConfig;
use crate::error::AppError;
use crate::tool_runtime::broker::BrokerToolCall;
use crate::tool_runtime::openai_compiler::{CompiledToolCall, CompilerDecision};
use crate::tool_runtime::protocol::{
    normalize_tool_call_ids, openai_request_to_context, openai_tool_choice_to_spec,
    openai_tools_to_specs, validate_tool_choice,
};

/// 工具决策来源模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecisionMode {
    /// 本地 broker 解析（标记 / 中文规则）命中。
    Broker,
    /// 外部编译器决策。
    Compiler,
    /// 未启用任何决策路径，直接按最终回答处理。
    None,
}

/// 工具决策的 token 消耗统计。
#[derive(Debug, Clone)]
pub struct ToolDecisionUsage {
    /// 决策过程消耗的输入 token 数（本地解析路径恒为 0）。
    pub prompt_tokens: u64,
    /// 决策过程消耗的输出 token 数（本地解析路径恒为 0）。
    pub completion_tokens: u64,
}

/// 工具决策元数据，用于响应体或日志透出。
#[derive(Debug, Clone)]
pub struct ToolDecisionMeta {
    /// 决策来源模式。
    pub mode: ToolDecisionMode,
    /// 决策使用的供应商标识（编译器 / broker / `"none"`）。
    pub provider: String,
    /// 决策使用的模型标识；未配置时为 `"rule-based"` 或 `"none"`。
    pub model: String,
    /// 决策使用的服务地址；未配置时为空串。
    pub base_url: String,
    /// 本次决策的 token 用量。
    pub usage: ToolDecisionUsage,
}

/// 基于主模型文本输出的工具决策结果。
#[derive(Debug, Clone)]
pub enum ToolTextDecision {
    /// 需要执行工具。
    ToolCalls {
        /// 校验通过、ID 已归一化的工具调用列表。
        calls: Vec<CompiledToolCall>,
        /// 决策元数据。
        meta: ToolDecisionMeta,
    },
    /// 直接给出最终回答。
    FinalAnswer {
        /// 决策元数据。
        meta: ToolDecisionMeta,
    },
}

fn runtime_provider(config: Option<&ToolRuntimeConfig>) -> String {
    config
        .map(|cfg| match cfg.provider {
            crate::config::ToolRuntimeProvider::Anthropic => "anthropic".to_string(),
            crate::config::ToolRuntimeProvider::OpenaiCompatible => "openai-compatible".to_string(),
        })
        .unwrap_or_else(|| "rule-based".to_string())
}

fn runtime_model(config: Option<&ToolRuntimeConfig>) -> String {
    config
        .and_then(|cfg| cfg.model.clone())
        .unwrap_or_else(|| "rule-based".to_string())
}

fn runtime_base_url(config: Option<&ToolRuntimeConfig>) -> String {
    config
        .and_then(|cfg| cfg.base_url.clone())
        .unwrap_or_default()
}

fn broker_calls_to_compiled(calls: Vec<BrokerToolCall>) -> Vec<CompiledToolCall> {
    calls
        .into_iter()
        .map(|call| CompiledToolCall {
            id: call.id,
            name: call.name,
            input: call.input,
        })
        .collect()
}

/// 根据主模型的文本输出决定工具调用。
///
/// 处理顺序：提取负载并检测显式意图 → 解析标记工具调用 → 解析中文规则
/// 意图（标记未命中时）→ 命中即校验 tool_choice 并归一化 ID 后返回；
/// 均未命中且运行时配置启用时，构建上下文并调用外部编译器，对编译结果
/// 逐个校验 schema 与 tool_choice 后返回；编译器未启用则按最终回答处理。
///
/// # 参数
/// - `http_client`：编译器路径使用的 HTTP 客户端。
/// - `runtime_config`：工具运行时配置；为 `None` 或未启用时跳过编译器。
/// - `req`：OpenAI Chat 请求（提供工具定义与 tool_choice）。
/// - `main_model_output`：主模型的原始输出文本。
///
/// # 返回
/// 决策结果：工具调用列表或最终回答，附带来源与用量元数据。
///
/// # 错误
/// 返回 `AppError::ToolProtocol`：标记解析失败、入参违反 schema、
/// tool_choice 约束不满足，或"有明确工具意图但未产生合法调用"。
/// 编译器路径的网络 / 上游错误按原样透传。
///
/// # 副作用
/// 走编译器路径时会发起一次外部 HTTP 调用并消耗其 token 配额。
pub async fn decide_openai_tools_from_text(
    http_client: reqwest::Client,
    runtime_config: Option<&ToolRuntimeConfig>,
    req: &crate::model::openai::ChatCompletionRequest,
    main_model_output: &str,
) -> Result<ToolTextDecision, AppError> {
    let tool_specs = req
        .tools
        .as_ref()
        .map(|tools| openai_tools_to_specs(tools.as_slice()))
        .unwrap_or_default();

    let tool_choice = openai_tool_choice_to_spec(req.tool_choice.as_ref());
    let payload_extraction = crate::tool_runtime::payload::extract_payloads(main_model_output);
    let decision_text = payload_extraction.sanitized_text.as_str();
    let explicit_intent = crate::tool_runtime::dialect::has_explicit_tool_intent(decision_text);

    let marked_calls =
        crate::tool_runtime::broker::parse_marked_tool_calls(decision_text, &tool_specs)
            .map_err(AppError::ToolProtocol)?;
    let rule_calls = if marked_calls.is_empty() {
        crate::tool_runtime::broker::parse_rule_tool_intent(decision_text, &tool_specs)
            .map_err(AppError::ToolProtocol)?
    } else {
        Vec::new()
    };

    let broker_calls = if !marked_calls.is_empty() {
        marked_calls
    } else {
        rule_calls
    };

    if !broker_calls.is_empty() {
        let mut calls = broker_calls_to_compiled(broker_calls);
        let actual_names = calls
            .iter()
            .map(|call| call.name.clone())
            .collect::<Vec<_>>();
        validate_tool_choice(tool_choice.as_ref(), &actual_names)?;
        normalize_tool_call_ids(calls.iter_mut().map(|call| &mut call.id));
        return Ok(ToolTextDecision::ToolCalls {
            calls,
            meta: ToolDecisionMeta {
                mode: ToolDecisionMode::Broker,
                provider: runtime_provider(runtime_config),
                model: runtime_model(runtime_config),
                base_url: runtime_base_url(runtime_config),
                usage: ToolDecisionUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                },
            },
        });
    }

    let Some(config) = runtime_config.filter(|cfg| cfg.enabled) else {
        validate_tool_choice(tool_choice.as_ref(), &[])?;
        if explicit_intent {
            return Err(AppError::ToolProtocol(
                "检测到明确工具意图，但没有生成合法工具调用".to_string(),
            ));
        }
        return Ok(ToolTextDecision::FinalAnswer {
            meta: ToolDecisionMeta {
                mode: ToolDecisionMode::None,
                provider: "none".to_string(),
                model: "none".to_string(),
                base_url: String::new(),
                usage: ToolDecisionUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                },
            },
        });
    };

    let mut context = openai_request_to_context(req, tool_specs)?;
    context.payloads = payload_extraction.payloads;
    let compiled = crate::tool_runtime::openai_compiler::compile_tool_decision_for_context(
        http_client,
        config,
        &context,
        decision_text,
    )
    .await?;

    let meta = ToolDecisionMeta {
        mode: ToolDecisionMode::Compiler,
        provider: runtime_provider(Some(config)),
        model: runtime_model(Some(config)),
        base_url: runtime_base_url(Some(config)),
        usage: ToolDecisionUsage {
            prompt_tokens: compiled.usage.prompt_tokens,
            completion_tokens: compiled.usage.completion_tokens,
        },
    };

    match compiled.decision {
        CompilerDecision::ToolCalls(calls) => {
            for call in &calls {
                crate::tool_runtime::validation::validate_tool_call(
                    &context.tools,
                    &call.name,
                    &call.input,
                )
                .map_err(AppError::ToolProtocol)?;
            }
            let mut calls = calls;
            let actual_names = calls
                .iter()
                .map(|call| call.name.clone())
                .collect::<Vec<_>>();
            validate_tool_choice(context.tool_choice.as_ref(), &actual_names)?;
            normalize_tool_call_ids(calls.iter_mut().map(|call| &mut call.id));
            Ok(ToolTextDecision::ToolCalls { calls, meta })
        }
        CompilerDecision::FinalAnswer => {
            validate_tool_choice(context.tool_choice.as_ref(), &[])?;
            if explicit_intent {
                Err(AppError::ToolProtocol(
                    "编译器未能转换明确的工具意图".to_string(),
                ))
            } else {
                Ok(ToolTextDecision::FinalAnswer { meta })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::openai::{ChatCompletionRequest, ChatMessage, ToolDef, ToolFunction};

    fn openai_req_with_bash_tool() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("列出当前目录".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: Some(vec![ToolDef {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "Bash".to_string(),
                    description: Some("执行命令".to_string()),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" }
                        },
                        "required": ["command"]
                    })),
                },
            }]),
            tool_choice: None,
            parallel_tool_calls: None,
        }
    }

    #[tokio::test]
    async fn test_decide_openai_tools_from_marked_output_without_compiler() {
        let req = openai_req_with_bash_tool();
        // 使用欧亿标记格式模拟工具调用
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let decision = decide_openai_tools_from_text(reqwest::Client::new(), None, &req, output)
            .await
            .unwrap();

        match decision {
            ToolTextDecision::ToolCalls { calls, meta } => {
                assert_eq!(meta.mode, ToolDecisionMode::Broker);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "Bash");
                assert_eq!(calls[0].input["command"], "pwd");
            }
            ToolTextDecision::FinalAnswer { .. } => panic!("expected tool calls"),
        }
    }

    #[tokio::test]
    async fn test_explicit_invalid_glm_call_returns_tool_protocol_error() {
        let req = openai_req_with_bash_tool();
        let error = decide_openai_tools_from_text(
            reqwest::Client::new(),
            None,
            &req,
            r#"<tool_call>Bash▁command▁params▁{"description":"missing command"}"#,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, AppError::ToolProtocol(_)));
    }

    #[tokio::test]
    async fn test_required_choice_rejects_final_answer_without_compiler() {
        let mut req = openai_req_with_bash_tool();
        req.tool_choice = Some(serde_json::json!("required"));

        let error = decide_openai_tools_from_text(
            reqwest::Client::new(),
            None,
            &req,
            "这是最终回答，没有工具调用。",
        )
        .await
        .unwrap_err();

        assert!(
            matches!(error, AppError::ToolProtocol(message) if message.contains("expected=required") && message.contains("actual=<none>"))
        );
    }

    #[tokio::test]
    async fn test_specific_choice_rejects_other_valid_marked_tool() {
        let mut req = openai_req_with_bash_tool();
        req.tools.as_mut().unwrap().push(ToolDef {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "Read".to_string(),
                description: Some("读取文件".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"file_path": {"type": "string"}},
                    "required": ["file_path"]
                })),
            },
        });
        req.tool_choice = Some(serde_json::json!({
            "type": "function",
            "function": {"name": "Read"}
        }));
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let error = decide_openai_tools_from_text(reqwest::Client::new(), None, &req, output)
            .await
            .unwrap_err();

        assert!(
            matches!(error, AppError::ToolProtocol(message) if message.contains("expected=Read") && message.contains("actual=Bash"))
        );
    }

    #[tokio::test]
    async fn test_required_choice_rejects_compiler_final_answer() {
        use axum::{Json, Router, routing::post};

        async fn final_answer() -> Json<serde_json::Value> {
            Json(serde_json::json!({
                "choices": [{"message": {"content": "no tool call"}}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 1}
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/chat/completions", post(final_answer)),
            )
            .await
            .unwrap();
        });
        let config = ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: crate::config::ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some(format!("http://{addr}")),
            api_key: Some("test-key".to_string()),
            api_key_env: None,
            model: Some("tool-model".to_string()),
            timeout_secs: 5,
        };
        let mut req = openai_req_with_bash_tool();
        req.tool_choice = Some(serde_json::json!("required"));

        let error = decide_openai_tools_from_text(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            Some(&config),
            &req,
            "请决定是否调用工具。",
        )
        .await
        .unwrap_err();

        assert!(
            matches!(error, AppError::ToolProtocol(message) if message.contains("expected=required") && message.contains("actual=<none>"))
        );
    }
}
