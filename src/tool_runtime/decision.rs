use crate::config::ToolRuntimeConfig;
use crate::error::AppError;
use crate::tool_runtime::broker::BrokerToolCall;
use crate::tool_runtime::openai_compiler::{CompiledToolCall, CompilerDecision};
use crate::tool_runtime::protocol::{openai_request_to_context, openai_tools_to_specs};

/// 工具决策来源模式
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecisionMode {
    Broker,
    Compiler,
    None,
}

/// 工具决策 token 消耗统计
#[derive(Debug, Clone)]
pub struct ToolDecisionUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// 工具决策元数据
#[derive(Debug, Clone)]
pub struct ToolDecisionMeta {
    pub mode: ToolDecisionMode,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub usage: ToolDecisionUsage,
}

/// 文本后工具决策结果
#[derive(Debug, Clone)]
pub enum ToolTextDecision {
    ToolCalls {
        calls: Vec<CompiledToolCall>,
        meta: ToolDecisionMeta,
    },
    FinalAnswer {
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

/// 根据主模型文本输出决定工具调用
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

    let payload_extraction = crate::tool_runtime::payload::extract_payloads(main_model_output);
    let decision_text = payload_extraction.sanitized_text.as_str();

    let marked_calls =
        crate::tool_runtime::broker::parse_marked_tool_calls(decision_text, &tool_specs)
            .map_err(AppError::BadRequest)?;
    let rule_calls = if marked_calls.is_empty() {
        crate::tool_runtime::broker::parse_rule_tool_intent(decision_text, &tool_specs)
            .map_err(AppError::BadRequest)?
    } else {
        Vec::new()
    };

    let broker_calls = if !marked_calls.is_empty() {
        marked_calls
    } else {
        rule_calls
    };

    if !broker_calls.is_empty() {
        return Ok(ToolTextDecision::ToolCalls {
            calls: broker_calls_to_compiled(broker_calls),
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
        CompilerDecision::ToolCalls(calls) => Ok(ToolTextDecision::ToolCalls { calls, meta }),
        CompilerDecision::FinalAnswer => Ok(ToolTextDecision::FinalAnswer { meta }),
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
}
