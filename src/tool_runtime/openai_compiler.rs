use crate::config::{ToolRuntimeConfig, ToolRuntimeProvider};
use crate::error::AppError;
use crate::model::anthropic::{AnthropicTool, MessagesRequest};
use crate::tool_runtime::protocol::{
    ToolChoiceSpec, ToolEndpointKind, ToolRuntimeRequestContext, ToolSpec,
};
use serde::{Deserialize, Serialize};

/// 编译后的工具调用结构
#[derive(Debug, Clone)]
pub struct CompiledToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// 编译器决策结果
#[derive(Debug, Clone)]
pub enum CompilerDecision {
    ToolCalls(Vec<CompiledToolCall>),
    FinalAnswer,
}

#[derive(Debug, Clone)]
pub struct CompilerUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct CompilerResult {
    pub decision: CompilerDecision,
    pub usage: CompilerUsage,
}

/// OpenAI-compatible tools 请求中的 tool 定义
#[derive(Debug, Clone, Serialize)]
struct OpenAiTool {
    r#type: String,
    function: OpenAiFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// OpenAI-compatible chat/completions 请求
#[derive(Debug, Clone, Serialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    tools: Vec<OpenAiTool>,
    tool_choice: OpenAiToolChoice,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum OpenAiToolChoice {
    String(String),
    Object {
        r#type: String,
        function: OpenAiToolChoiceFunction,
    },
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiToolChoiceFunction {
    name: String,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

/// OpenAI-compatible chat/completions 响应
#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiToolCallFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicCompilerRequest {
    model: String,
    messages: Vec<AnthropicCompilerMessage>,
    system: String,
    max_tokens: u64,
    tools: Vec<AnthropicTool>,
    tool_choice: AnthropicCompilerToolChoice,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicCompilerMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum AnthropicCompilerToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "tool")]
    Tool { name: String },
}

#[derive(Debug, Deserialize)]
struct AnthropicCompilerResponse {
    content: Vec<AnthropicCompilerContentBlock>,
    usage: Option<AnthropicCompilerUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicCompilerUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicCompilerContentBlock {
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

/// 解析 OpenAI tool_calls 为 CompiledToolCall
fn parse_openai_tool_calls(
    tool_calls: &[OpenAiToolCall],
    valid_tool_names: &[String],
) -> Result<Vec<CompiledToolCall>, AppError> {
    let mut compiled_calls = Vec::new();

    for call in tool_calls {
        if !valid_tool_names.contains(&call.function.name) {
            return Err(AppError::BadRequest(format!(
                "未知工具名: {}，有效工具: {}",
                call.function.name,
                valid_tool_names.join(", ")
            )));
        }

        let input: serde_json::Value =
            serde_json::from_str(&call.function.arguments).map_err(|_| {
                AppError::BadRequest(format!(
                    "工具 {} 参数 JSON 解析失败: {}",
                    call.function.name, call.function.arguments
                ))
            })?;

        if !input.is_object() {
            return Err(AppError::BadRequest(format!(
                "工具 {} 参数必须是 JSON object",
                call.function.name
            )));
        }

        compiled_calls.push(CompiledToolCall {
            id: call.id.clone(),
            name: call.function.name.clone(),
            input,
        });
    }

    Ok(compiled_calls)
}

fn parse_anthropic_tool_uses(
    content: &[AnthropicCompilerContentBlock],
    valid_tool_names: &[String],
) -> Result<Vec<CompiledToolCall>, AppError> {
    let mut compiled_calls = Vec::new();

    for block in content {
        if let AnthropicCompilerContentBlock::ToolUse { id, name, input } = block {
            if !valid_tool_names.contains(name) {
                return Err(AppError::BadRequest(format!(
                    "未知工具名: {}，有效工具: {}",
                    name,
                    valid_tool_names.join(", ")
                )));
            }
            if !input.is_object() {
                return Err(AppError::BadRequest(format!(
                    "工具 {} 参数必须是 JSON object",
                    name
                )));
            }
            compiled_calls.push(CompiledToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
        }
    }

    Ok(compiled_calls)
}

/// 带 payload 的工具决策编译（旧函数委托至此）
pub async fn compile_tool_decision_with_usage_and_payloads(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    req: &MessagesRequest,
    ouyi_output: &str,
    payloads: Vec<crate::tool_runtime::payload::ToolPayload>,
) -> Result<CompilerResult, AppError> {
    let tool_specs: Vec<ToolSpec> = req
        .tools
        .as_ref()
        .map(|tools| crate::tool_runtime::protocol::anthropic_tools_to_specs(tools.as_slice()))
        .unwrap_or_default();

    let transcript =
        crate::tool_runtime::context::from_anthropic_messages(req).map_err(AppError::BadRequest)?;

    let context = ToolRuntimeRequestContext {
        endpoint: ToolEndpointKind::Anthropic,
        model: req.model.clone(),
        transcript,
        tools: tool_specs,
        tool_choice: crate::tool_runtime::protocol::anthropic_tool_choice_to_spec(
            req.tool_choice.as_ref(),
        ),
        payloads,
    };

    compile_tool_decision_for_context(http_client, config, &context, ouyi_output).await
}

/// 基于协议无关上下文编译工具决策
/// OpenAI 端点只使用 OpenAI-compatible 工具模型
/// Anthropic 端点根据配置选择 provider
pub async fn compile_tool_decision_for_context(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    context: &ToolRuntimeRequestContext,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    if context.tools.is_empty() {
        return Ok(CompilerResult {
            decision: CompilerDecision::FinalAnswer,
            usage: CompilerUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
            },
        });
    }

    match context.endpoint {
        ToolEndpointKind::OpenAi => {
            compile_for_openai_endpoint(http_client, config, context, ouyi_output).await
        }
        ToolEndpointKind::Anthropic => {
            compile_for_anthropic_endpoint(http_client, config, context, ouyi_output).await
        }
    }
}

/// 为 OpenAI 端点编译工具决策，只使用 OpenAI-compatible 工具模型
async fn compile_for_openai_endpoint(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    context: &ToolRuntimeRequestContext,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    let valid_tool_names: Vec<String> = context.tools.iter().map(|t| t.name.clone()).collect();
    let compiler_specs = crate::tool_runtime::schema::compiler_tool_specs(&context.tools);
    let openai_tools = convert_tool_specs_to_openai(&compiler_specs);

    let tool_choice = context
        .tool_choice
        .as_ref()
        .map(|tc| convert_tool_choice_spec_to_openai(tc))
        .unwrap_or_else(|| OpenAiToolChoice::String("auto".to_string()));

    let model = config
        .model
        .as_ref()
        .ok_or_else(|| AppError::Config("openai_tool_runtime.model 未配置".to_string()))?;

    let base_url = config
        .base_url
        .as_ref()
        .ok_or_else(|| AppError::Config("openai_tool_runtime.base_url 未配置".to_string()))?;
    crate::tool_runtime::provider::validate_compiler_base_url(base_url)
        .map_err(AppError::ToolRecursion)?;

    let api_key = config.get_api_key()?;

    let user_message = build_compiler_user_message_from_context(context, ouyi_output);

    let openai_request = OpenAiChatRequest {
        model: model.clone(),
        messages: vec![
            OpenAiMessage {
                role: "system".to_string(),
                content: "你是工具调用编译器。你的唯一任务是将 Ouyi 主脑输出中的工具意图转换为结构化的 tool_calls。如果 Ouyi 输出明确表示需要调用工具，你必须返回对应的 tool_calls；如果 Ouyi 输出是最终回答或不需要工具，则不返回 tool_calls。不要添加任何额外解释。".to_string(),
            },
            OpenAiMessage {
                role: "user".to_string(),
                content: user_message,
            },
        ],
        tools: openai_tools,
        tool_choice,
    };

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = http_client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header(
            crate::tool_runtime::provider::TOOL_RUNTIME_HOP_HEADER,
            crate::tool_runtime::provider::TOOL_RUNTIME_HOP_VALUE,
        )
        .header("Content-Type", "application/json")
        .json(&openai_request)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AppError::Upstream(format!("编译器请求失败: {}", e)))?;

    let body = response
        .text()
        .await
        .map_err(|e| AppError::Upstream(format!("读取编译器响应失败: {}", e)))?;

    tracing::debug!(response_bytes = body.len(), "已读取编译器响应");

    let openai_response: OpenAiChatResponse = serde_json::from_str(&body)
        .map_err(|e| AppError::Upstream(format!("编译器响应解析失败: {}", e)))?;
    let usage = CompilerUsage {
        prompt_tokens: openai_response
            .usage
            .as_ref()
            .and_then(|u| u.prompt_tokens)
            .unwrap_or(0),
        completion_tokens: openai_response
            .usage
            .as_ref()
            .and_then(|u| u.completion_tokens)
            .unwrap_or(0),
    };

    let choice = openai_response
        .choices
        .first()
        .ok_or_else(|| AppError::Upstream("编译器响应缺少 choices".to_string()))?;

    if let Some(tool_calls) = &choice.message.tool_calls {
        if tool_calls.is_empty() {
            return Ok(CompilerResult {
                decision: CompilerDecision::FinalAnswer,
                usage,
            });
        }

        match parse_openai_tool_calls(tool_calls, &valid_tool_names) {
            Ok(calls) if !calls.is_empty() => {
                match materialize_compiled_calls(calls, &context.payloads)
                    .and_then(|calls| validate_compiled_calls(calls, &context.tools))
                {
                    Ok(calls) => Ok(CompilerResult {
                        decision: CompilerDecision::ToolCalls(calls),
                        usage,
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "首次 tool_calls 参数不符合工具 schema，尝试结构化重试");
                        retry_with_structured_prompt(
                            http_client,
                            config,
                            &openai_request,
                            &context.tools,
                            &context.payloads,
                        )
                        .await
                    }
                }
            }
            Ok(_) => Ok(CompilerResult {
                decision: CompilerDecision::FinalAnswer,
                usage,
            }),
            Err(e) => {
                tracing::warn!(error = %e, "首次解析 tool_calls 失败，尝试结构化重试");
                retry_with_structured_prompt(
                    http_client,
                    config,
                    &openai_request,
                    &context.tools,
                    &context.payloads,
                )
                .await
            }
        }
    } else {
        Ok(CompilerResult {
            decision: CompilerDecision::FinalAnswer,
            usage,
        })
    }
}

/// 为 Anthropic 端点编译工具决策，根据配置选择 provider
async fn compile_for_anthropic_endpoint(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    context: &ToolRuntimeRequestContext,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    match config.provider {
        ToolRuntimeProvider::OpenaiCompatible => {
            compile_for_openai_endpoint(http_client, config, context, ouyi_output).await
        }
        ToolRuntimeProvider::Anthropic => {
            compile_for_anthropic_provider(http_client, config, context, ouyi_output).await
        }
    }
}

/// 使用 Anthropic provider 编译工具决策
async fn compile_for_anthropic_provider(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    context: &ToolRuntimeRequestContext,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    let valid_tool_names: Vec<String> = context.tools.iter().map(|t| t.name.clone()).collect();
    let model = config
        .model
        .as_ref()
        .ok_or_else(|| AppError::Config("tool_runtime.model 未配置".to_string()))?;
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com")
        .trim_end_matches('/');
    let base_url = base_url.strip_suffix("/v1").unwrap_or(base_url);
    crate::tool_runtime::provider::validate_compiler_base_url(base_url)
        .map_err(AppError::ToolRecursion)?;
    let api_key = config.get_api_key()?;

    let compiler_specs = crate::tool_runtime::schema::compiler_tool_specs(&context.tools);
    let tools = convert_tool_specs_to_anthropic(&compiler_specs);
    let tool_choice = convert_tool_choice_spec_to_anthropic(context.tool_choice.as_ref());

    let request = AnthropicCompilerRequest {
        model: model.clone(),
        system: "你是 Claude Code 工具协议编译器。你的任务是把欧亿主脑已经表达出的工具意图转换为结构化 tool_use。报告正文、代码正文、文件正文必须来自欧亿 payload 引用；长文本字段使用 content_ref、old_string_ref、new_string_ref，不生成或改写正文。没有明确工具意图时不调用工具。不要添加解释。".to_string(),
        messages: vec![AnthropicCompilerMessage {
            role: "user".to_string(),
            content: build_compiler_user_message_from_context(context, ouyi_output),
        }],
        max_tokens: 512,
        tools,
        tool_choice,
    };

    let url = format!("{}/v1/messages", base_url);
    let response = http_client
        .post(&url)
        .header("x-api-key", api_key)
        .header(
            crate::tool_runtime::provider::TOOL_RUNTIME_HOP_HEADER,
            crate::tool_runtime::provider::TOOL_RUNTIME_HOP_VALUE,
        )
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&request)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AppError::Upstream(format!("编译器请求失败: {}", e)))?;

    let body = response
        .text()
        .await
        .map_err(|e| AppError::Upstream(format!("读取编译器响应失败: {}", e)))?;

    tracing::debug!(response_bytes = body.len(), "已读取 Anthropic 编译器响应");

    let anthropic_response: AnthropicCompilerResponse = serde_json::from_str(&body)
        .map_err(|e| AppError::Upstream(format!("编译器响应解析失败: {}", e)))?;
    let usage = CompilerUsage {
        prompt_tokens: anthropic_response
            .usage
            .as_ref()
            .and_then(|u| u.input_tokens)
            .unwrap_or(0),
        completion_tokens: anthropic_response
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0),
    };

    let calls = parse_anthropic_tool_uses(&anthropic_response.content, &valid_tool_names)?;
    if calls.is_empty() {
        Ok(CompilerResult {
            decision: CompilerDecision::FinalAnswer,
            usage,
        })
    } else {
        let calls = materialize_compiled_calls(calls, &context.payloads)?;
        Ok(CompilerResult {
            decision: CompilerDecision::ToolCalls(calls),
            usage,
        })
    }
}

/// 将 ToolSpec 转换为 OpenAI 工具格式
fn convert_tool_specs_to_openai(specs: &[ToolSpec]) -> Vec<OpenAiTool> {
    specs
        .iter()
        .map(|spec| OpenAiTool {
            r#type: "function".to_string(),
            function: OpenAiFunction {
                name: spec.name.clone(),
                description: spec.description.clone(),
                parameters: spec.input_schema.clone(),
            },
        })
        .collect()
}

/// 将 ToolSpec 转换为 Anthropic 工具格式
fn convert_tool_specs_to_anthropic(specs: &[ToolSpec]) -> Vec<AnthropicTool> {
    specs
        .iter()
        .map(|spec| AnthropicTool {
            name: spec.name.clone(),
            description: spec.description.clone(),
            input_schema: spec.input_schema.clone(),
        })
        .collect()
}

/// 将 ToolChoiceSpec 转换为 OpenAI tool_choice
fn convert_tool_choice_spec_to_openai(spec: &ToolChoiceSpec) -> OpenAiToolChoice {
    match spec {
        ToolChoiceSpec::Auto => OpenAiToolChoice::String("auto".to_string()),
        ToolChoiceSpec::Required => OpenAiToolChoice::String("required".to_string()),
        ToolChoiceSpec::Tool(name) => OpenAiToolChoice::Object {
            r#type: "function".to_string(),
            function: OpenAiToolChoiceFunction { name: name.clone() },
        },
    }
}

/// 将 ToolChoiceSpec 转换为 Anthropic tool_choice
fn convert_tool_choice_spec_to_anthropic(
    spec: Option<&ToolChoiceSpec>,
) -> AnthropicCompilerToolChoice {
    match spec {
        Some(ToolChoiceSpec::Auto) | None => AnthropicCompilerToolChoice::Auto,
        Some(ToolChoiceSpec::Required) => AnthropicCompilerToolChoice::Any,
        Some(ToolChoiceSpec::Tool(name)) => {
            AnthropicCompilerToolChoice::Tool { name: name.clone() }
        }
    }
}

/// 基于上下文构建编译器用户消息
fn build_compiler_user_message_from_context(
    context: &ToolRuntimeRequestContext,
    ouyi_output: &str,
) -> String {
    let mut content = String::new();

    content.push_str("以下是当前对话上下文：\n\n");

    if !context.transcript.is_empty() {
        content.push_str(&crate::tool_runtime::context::render_for_compiler(
            &context.transcript,
        ));
        content.push('\n');
    }

    content.push_str("\nOuyi 主脑输出：\n");
    content.push_str(ouyi_output);
    content.push('\n');

    content.push_str(concat!(
        "\n请只输出工具调用，不要输出最终回答。",
        "如果上述内容表达了工具意图，请选择最匹配的可用工具并生成合法参数。",
        "如果没有可执行工具意图，请不要调用任何工具。"
    ));

    if !context.payloads.is_empty() {
        content.push_str("\npayload 可用列表：\n");
        for payload in &context.payloads {
            content.push_str(&format!(
                "- id: {}, bytes: {}\n",
                payload.id, payload.byte_len
            ));
        }
        content.push_str(
            "长文本字段必须引用 payload id，例如 content_ref、old_string_ref、new_string_ref。\n",
        );
    }

    content
}

/// 结构化重试：使用更明确的提示重新请求编译器
async fn retry_with_structured_prompt(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    original_request: &OpenAiChatRequest,
    tool_specs: &[ToolSpec],
    payloads: &[crate::tool_runtime::payload::ToolPayload],
) -> Result<CompilerResult, AppError> {
    let valid_tool_names = tool_specs
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let retry_request = build_retry_request(original_request, &valid_tool_names);

    let base_url = config
        .base_url
        .as_ref()
        .ok_or_else(|| AppError::Config("tool_runtime.base_url 未配置".to_string()))?;
    crate::tool_runtime::provider::validate_compiler_base_url(base_url)
        .map_err(AppError::ToolRecursion)?;

    let api_key = config.get_api_key()?;

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = http_client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header(
            crate::tool_runtime::provider::TOOL_RUNTIME_HOP_HEADER,
            crate::tool_runtime::provider::TOOL_RUNTIME_HOP_VALUE,
        )
        .header("Content-Type", "application/json")
        .json(&retry_request)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AppError::Upstream(format!("编译器重试请求失败: {}", e)))?;

    let body = response
        .text()
        .await
        .map_err(|e| AppError::Upstream(format!("读取编译器重试响应失败: {}", e)))?;

    tracing::debug!(response_bytes = body.len(), "已读取编译器重试响应");

    let openai_response: OpenAiChatResponse = serde_json::from_str(&body)
        .map_err(|e| AppError::Upstream(format!("编译器重试响应解析失败: {}", e)))?;
    let usage = CompilerUsage {
        prompt_tokens: openai_response
            .usage
            .as_ref()
            .and_then(|u| u.prompt_tokens)
            .unwrap_or(0),
        completion_tokens: openai_response
            .usage
            .as_ref()
            .and_then(|u| u.completion_tokens)
            .unwrap_or(0),
    };

    let choice = openai_response
        .choices
        .first()
        .ok_or_else(|| AppError::Upstream("编译器重试响应缺少 choices".to_string()))?;

    if let Some(tool_calls) = &choice.message.tool_calls {
        if tool_calls.is_empty() {
            return Ok(CompilerResult {
                decision: CompilerDecision::FinalAnswer,
                usage,
            });
        }

        let compiled_calls = parse_openai_tool_calls(tool_calls, &valid_tool_names)?;
        if compiled_calls.is_empty() {
            Ok(CompilerResult {
                decision: CompilerDecision::FinalAnswer,
                usage,
            })
        } else {
            let compiled_calls = materialize_compiled_calls(compiled_calls, payloads)?;
            let compiled_calls = validate_compiled_calls(compiled_calls, tool_specs)?;
            Ok(CompilerResult {
                decision: CompilerDecision::ToolCalls(compiled_calls),
                usage,
            })
        }
    } else {
        Ok(CompilerResult {
            decision: CompilerDecision::FinalAnswer,
            usage,
        })
    }
}

fn build_retry_request(
    original_request: &OpenAiChatRequest,
    valid_tool_names: &[String],
) -> OpenAiChatRequest {
    let mut messages = original_request.messages.clone();
    messages.push(OpenAiMessage {
        role: "user".to_string(),
        content: format!(
            "上一次 tool_calls 参数无法解析。请基于前面的 Ouyi 主脑输出重新返回 tool_calls。可用工具：{}。arguments 必须是合法 JSON object；长文本字段继续使用已有 payload 引用。",
            valid_tool_names.join(", ")
        ),
    });

    OpenAiChatRequest {
        model: original_request.model.clone(),
        messages,
        tools: original_request.tools.clone(),
        tool_choice: original_request.tool_choice.clone(),
    }
}

fn materialize_compiled_calls(
    mut calls: Vec<CompiledToolCall>,
    payloads: &[crate::tool_runtime::payload::ToolPayload],
) -> Result<Vec<CompiledToolCall>, AppError> {
    for call in &mut calls {
        crate::tool_runtime::materialize::materialize_payload_refs(&mut call.input, payloads)
            .map_err(|e| AppError::Upstream(format!("工具参数 payload 物化失败: {e}")))?;
    }
    Ok(calls)
}

fn validate_compiled_calls(
    calls: Vec<CompiledToolCall>,
    tool_specs: &[ToolSpec],
) -> Result<Vec<CompiledToolCall>, AppError> {
    for call in &calls {
        crate::tool_runtime::validation::validate_tool_call(tool_specs, &call.name, &call.input)
            .map_err(AppError::ToolProtocol)?;
    }
    Ok(calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_openai_tool_calls_valid() {
        let tool_calls = vec![OpenAiToolCall {
            id: "call_123".to_string(),
            function: OpenAiToolCallFunction {
                name: "Read".to_string(),
                arguments: "{\"query\": \"test\"}".to_string(),
            },
        }];
        let valid_names = vec!["Read".to_string()];

        let result = parse_openai_tool_calls(&tool_calls, &valid_names);
        assert!(result.is_ok());
        let calls = result.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_123");
        assert_eq!(calls[0].name, "Read");
    }

    #[test]
    fn test_parse_openai_tool_calls_unknown_name() {
        let tool_calls = vec![OpenAiToolCall {
            id: "call_123".to_string(),
            function: OpenAiToolCallFunction {
                name: "Unknown".to_string(),
                arguments: "{}".to_string(),
            },
        }];
        let valid_names = vec!["Read".to_string()];

        let result = parse_openai_tool_calls(&tool_calls, &valid_names);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_openai_tool_calls_invalid_json() {
        let tool_calls = vec![OpenAiToolCall {
            id: "call_123".to_string(),
            function: OpenAiToolCallFunction {
                name: "Read".to_string(),
                arguments: "not json".to_string(),
            },
        }];
        let valid_names = vec!["Read".to_string()];

        let result = parse_openai_tool_calls(&tool_calls, &valid_names);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_openai_tool_calls_non_object() {
        let tool_calls = vec![OpenAiToolCall {
            id: "call_123".to_string(),
            function: OpenAiToolCallFunction {
                name: "Read".to_string(),
                arguments: "\"string\"".to_string(),
            },
        }];
        let valid_names = vec!["Read".to_string()];

        let result = parse_openai_tool_calls(&tool_calls, &valid_names);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_anthropic_tool_uses_valid() {
        let content = vec![AnthropicCompilerContentBlock::ToolUse {
            id: "toolu_123".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"query": "test"}),
        }];
        let valid_names = vec!["Read".to_string()];

        let calls = parse_anthropic_tool_uses(&content, &valid_names).unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_123");
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].input["query"], "test");
    }

    #[test]
    fn test_parse_anthropic_tool_uses_unknown_name() {
        let content = vec![AnthropicCompilerContentBlock::ToolUse {
            id: "toolu_123".to_string(),
            name: "Unknown".to_string(),
            input: serde_json::json!({"query": "test"}),
        }];
        let valid_names = vec!["Read".to_string()];

        assert!(parse_anthropic_tool_uses(&content, &valid_names).is_err());
    }

    #[test]
    fn test_compiler_user_message_lists_payload_refs_without_body() {
        let context = crate::tool_runtime::protocol::ToolRuntimeRequestContext {
            endpoint: crate::tool_runtime::protocol::ToolEndpointKind::Anthropic,
            model: "claude-test".to_string(),
            transcript: vec![crate::tool_runtime::context::TranscriptItem::UserText {
                content: "写报告".to_string(),
            }],
            tool_choice: None,
            tools: vec![],
            payloads: vec![crate::tool_runtime::payload::ToolPayload {
                id: "p1".to_string(),
                body: "# 私有报告正文".to_string(),
                byte_len: "# 私有报告正文".len(),
            }],
        };

        let message = build_compiler_user_message_from_context(
            &context,
            "Ouyi 输出：<ouyi_payload_ref id=\"p1\" bytes=\"20\" />",
        );

        assert!(message.contains("payload 可用列表"));
        assert!(message.contains("p1"));
        assert!(message.contains("bytes"));
        assert!(!message.contains("# 私有报告正文"));
    }

    #[test]
    fn test_retry_request_preserves_original_compiler_context() {
        let original = OpenAiChatRequest {
            model: "tool-model".to_string(),
            messages: vec![
                OpenAiMessage {
                    role: "system".to_string(),
                    content: "协议编译器".to_string(),
                },
                OpenAiMessage {
                    role: "user".to_string(),
                    content: concat!(
                        "Ouyi 主脑输出：<ouyi_payload_ref id=\"p1\" bytes=\"20\" />\n",
                        "payload 可用列表：\n",
                        "- id: p1, bytes: 20\n"
                    )
                    .to_string(),
                },
            ],
            tools: vec![
                convert_tool_specs_to_openai(&[ToolSpec {
                    name: "Write".to_string(),
                    description: "Write file".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "file_path": {"type": "string"},
                            "content_ref": {"type": "string"}
                        },
                        "required": ["file_path", "content_ref"]
                    }),
                }])
                .remove(0),
            ],
            tool_choice: OpenAiToolChoice::String("auto".to_string()),
        };

        let retry = build_retry_request(&original, &["Write".to_string()]);
        let joined_messages = retry
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(joined_messages.contains("Ouyi 主脑输出"));
        assert!(joined_messages.contains("payload 可用列表"));
        assert!(joined_messages.contains("p1"));
        assert!(joined_messages.contains("Write"));
    }

    #[tokio::test]
    async fn test_retry_materializes_payload_refs() {
        use axum::{Json, Router, routing::post};
        use tokio::net::TcpListener;

        async fn handler(headers: axum::http::HeaderMap) -> Json<serde_json::Value> {
            assert_eq!(
                headers
                    .get(crate::tool_runtime::provider::TOOL_RUNTIME_HOP_HEADER)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::tool_runtime::provider::TOOL_RUNTIME_HOP_VALUE)
            );
            Json(serde_json::json!({
                "choices": [{
                    "message": {
                        "tool_calls": [{
                            "id": "call_retry",
                            "function": {
                                "name": "Write",
                                "arguments": "{\"file_path\":\"docs/report.md\",\"content_ref\":\"p1\"}"
                            }
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }))
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/chat/completions", post(handler)),
            )
            .await
            .unwrap();
        });

        let tool_specs = vec![ToolSpec {
            name: "Write".to_string(),
            description: "Write file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"]
            }),
        }];
        let original = OpenAiChatRequest {
            model: "tool-model".to_string(),
            messages: vec![OpenAiMessage {
                role: "user".to_string(),
                content: "Ouyi 主脑输出：<ouyi_payload_ref id=\"p1\" bytes=\"1500\" />".to_string(),
            }],
            tools: convert_tool_specs_to_openai(&crate::tool_runtime::schema::compiler_tool_specs(
                &tool_specs,
            )),
            tool_choice: OpenAiToolChoice::String("auto".to_string()),
        };
        let body = "x".repeat(1500);
        let payloads = vec![crate::tool_runtime::payload::ToolPayload {
            id: "p1".to_string(),
            body: body.clone(),
            byte_len: body.len(),
        }];
        let config = ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some(format!("http://{}", addr)),
            api_key: Some("test-key".to_string()),
            api_key_env: None,
            model: Some("tool-model".to_string()),
            timeout_secs: 5,
        };

        let result = retry_with_structured_prompt(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            &config,
            &original,
            &tool_specs,
            &payloads,
        )
        .await
        .unwrap();

        match result.decision {
            CompilerDecision::ToolCalls(calls) => {
                assert_eq!(calls[0].input["content"], body);
                assert!(calls[0].input.get("content_ref").is_none());
            }
            CompilerDecision::FinalAnswer => panic!("expected retry tool call"),
        }
    }

    #[tokio::test]
    async fn test_schema_invalid_compiler_call_is_retried_once() {
        use axum::{Json, Router, extract::State, routing::post};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::net::TcpListener;

        async fn handler(State(requests): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
            let attempt = requests.fetch_add(1, Ordering::SeqCst);
            let arguments = if attempt == 0 {
                r#"{"file_path":"alpha无法调用工具"}"#
            } else {
                r#"{"file_path":"alpha"}"#
            };
            Json(serde_json::json!({
                "choices": [{
                    "message": {
                        "tool_calls": [{
                            "id": "call_retry",
                            "function": {
                                "name": "Read",
                                "arguments": arguments
                            }
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }))
        }

        let requests = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn({
            let requests = requests.clone();
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/chat/completions", post(handler))
                        .with_state(requests),
                )
                .await
                .unwrap();
            }
        });

        let context = ToolRuntimeRequestContext {
            endpoint: ToolEndpointKind::Anthropic,
            model: "gpt-test".to_string(),
            transcript: vec![crate::tool_runtime::context::TranscriptItem::UserText {
                content: "读取 alpha".to_string(),
            }],
            tools: vec![ToolSpec {
                name: "Read".to_string(),
                description: "Read deterministic input".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string", "enum": ["alpha"]}
                    },
                    "required": ["file_path"],
                    "additionalProperties": false
                }),
            }],
            tool_choice: Some(ToolChoiceSpec::Required),
            payloads: Vec::new(),
        };
        let config = ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some(format!("http://{}", addr)),
            api_key: Some("test-key".to_string()),
            api_key_env: None,
            model: Some("tool-model".to_string()),
            timeout_secs: 5,
        };

        let result = compile_tool_decision_for_context(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            &config,
            &context,
            "需要读取 alpha。",
        )
        .await
        .unwrap();

        assert_eq!(requests.load(Ordering::SeqCst), 2);
        match result.decision {
            CompilerDecision::ToolCalls(calls) => {
                assert_eq!(calls[0].input["file_path"], "alpha");
            }
            CompilerDecision::FinalAnswer => panic!("expected repaired tool call"),
        }
    }

    #[tokio::test]
    async fn test_anthropic_compiler_receives_real_forced_tool_choice() {
        use axum::{Json, Router, routing::post};
        use tokio::net::TcpListener;

        async fn handler(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            assert_eq!(body["tool_choice"]["type"], "any");
            Json(serde_json::json!({
                "content": [],
                "usage": {"input_tokens": 1, "output_tokens": 0}
            }))
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/messages", post(handler)))
                .await
                .unwrap();
        });
        let req = MessagesRequest {
            model: "claude-test".to_string(),
            messages: vec![crate::model::anthropic::AnthropicMessage {
                role: "user".to_string(),
                content: crate::model::anthropic::AnthropicContent::Text("run".to_string()),
            }],
            system: None,
            max_tokens: 128,
            stream: Some(false),
            tools: Some(vec![AnthropicTool {
                name: "Bash".to_string(),
                description: "Run shell command".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: Some(crate::model::anthropic::ToolChoice::Any),
        };
        let config = ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: ToolRuntimeProvider::Anthropic,
            base_url: Some(format!("http://{addr}")),
            api_key: Some("test-key".to_string()),
            api_key_env: None,
            model: Some("tool-model".to_string()),
            timeout_secs: 5,
        };

        let result = compile_tool_decision_with_usage_and_payloads(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            &config,
            &req,
            "no marked call",
            Vec::new(),
        )
        .await
        .unwrap();

        assert!(matches!(result.decision, CompilerDecision::FinalAnswer));
    }
}
