use crate::config::{ToolRuntimeConfig, ToolRuntimeProvider};
use crate::error::AppError;
use crate::model::anthropic::{AnthropicTool, MessagesRequest, ToolChoice};
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

/// 将 Anthropic tools schema 转换为 OpenAI-compatible function tools
fn convert_anthropic_tools_to_openai(anthropic_tools: &[AnthropicTool]) -> Vec<OpenAiTool> {
    anthropic_tools
        .iter()
        .map(|tool| OpenAiTool {
            r#type: "function".to_string(),
            function: OpenAiFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        })
        .collect()
}

/// 将 Anthropic tool_choice 转换为 OpenAI-compatible tool_choice
fn convert_anthropic_tool_choice_to_openai(
    tool_choice: &ToolChoice,
    _tools: &[AnthropicTool],
) -> OpenAiToolChoice {
    match tool_choice {
        ToolChoice::Auto => OpenAiToolChoice::String("auto".to_string()),
        ToolChoice::Any => OpenAiToolChoice::String("required".to_string()),
        ToolChoice::Tool { name } => OpenAiToolChoice::Object {
            r#type: "function".to_string(),
            function: OpenAiToolChoiceFunction { name: name.clone() },
        },
    }
}

fn convert_anthropic_tool_choice_to_anthropic(
    tool_choice: Option<&ToolChoice>,
) -> AnthropicCompilerToolChoice {
    match tool_choice {
        Some(ToolChoice::Any) => AnthropicCompilerToolChoice::Any,
        Some(ToolChoice::Tool { name }) => AnthropicCompilerToolChoice::Tool { name: name.clone() },
        Some(ToolChoice::Auto) | None => AnthropicCompilerToolChoice::Auto,
    }
}

/// 构建编译器请求的用户消息内容
pub(crate) fn build_compiler_user_message(req: &MessagesRequest, ouyi_output: &str) -> String {
    let mut content = String::new();

    content.push_str("以下是当前对话上下文：\n\n");

    if let Some(latest_user_msg) = req.messages.iter().rev().find(|m| m.role == "user") {
        content.push_str("用户最新消息：\n");
        match &latest_user_msg.content {
            crate::model::anthropic::AnthropicContent::Text(text) => {
                content.push_str(text);
                content.push('\n');
            }
            crate::model::anthropic::AnthropicContent::Blocks(blocks) => {
                for block in blocks.iter().rev() {
                    if let crate::model::anthropic::ContentBlock::Text { text } = block {
                        content.push_str(text);
                        content.push('\n');
                        break;
                    }
                }
            }
        }
    }

    if let Some(latest_tool_result) = crate::tool_runtime::latest_tool_result_id(req) {
        if let crate::model::anthropic::AnthropicContent::Blocks(blocks) = &req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .unwrap()
            .content
        {
            for block in blocks.iter().rev() {
                if let crate::model::anthropic::ContentBlock::ToolResult {
                    tool_use_id,
                    content: result_content,
                } = block
                {
                    if tool_use_id == &latest_tool_result {
                        content.push_str("\n最近的工具返回结果：\n");
                        content.push_str(result_content);
                        content.push('\n');
                        break;
                    }
                }
            }
        }
    }

    content.push_str("\nOuyi 主脑输出：\n");
    content.push_str(ouyi_output);
    content.push('\n');

    content.push_str(concat!(
        "\n请只输出工具调用，不要输出最终回答。",
        "如果上述内容表达了工具意图，请选择最匹配的可用工具并生成合法参数。",
        "如果没有可执行工具意图，请不要调用任何工具。"
    ));

    content
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

/// 调用 OpenAI-compatible API 编译工具决策并返回用量
pub async fn compile_tool_decision_with_usage(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    req: &MessagesRequest,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    match config.provider {
        ToolRuntimeProvider::OpenaiCompatible => {
            compile_tool_decision_openai(http_client, config, req, ouyi_output).await
        }
        ToolRuntimeProvider::Anthropic => {
            compile_tool_decision_anthropic(http_client, config, req, ouyi_output).await
        }
    }
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
    let openai_tools = convert_tool_specs_to_openai(&context.tools);

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

    tracing::debug!(body = %body, "编译器响应原文");

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
            Ok(calls) if !calls.is_empty() => Ok(CompilerResult {
                decision: CompilerDecision::ToolCalls(calls),
                usage,
            }),
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
                    &valid_tool_names,
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
    let api_key = config.get_api_key()?;

    let tools = convert_tool_specs_to_anthropic(&context.tools);
    let tool_choice = convert_tool_choice_spec_to_anthropic(context.tool_choice.as_ref());

    let request = AnthropicCompilerRequest {
        model: model.clone(),
        system: "你是工具调用编译器。你的唯一任务是将 Ouyi 主脑输出中的工具意图转换为结构化的 tool_use。如果 Ouyi 输出明确表示需要调用工具，你必须调用对应工具；如果 Ouyi 输出是最终回答或不需要工具，则不要调用工具。不要添加任何额外解释。".to_string(),
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

    tracing::debug!(body = %body, "Anthropic 编译器响应原文");

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

    if let Some(user_text) = &context.latest_user_text {
        content.push_str("用户最新消息：\n");
        content.push_str(user_text);
        content.push('\n');
    }

    if let Some(tool_result) = &context.latest_tool_result {
        content.push_str("\n最近的工具返回结果：\n");
        content.push_str(tool_result);
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

    content
}

async fn compile_tool_decision_openai(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    req: &MessagesRequest,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    let tools = req
        .tools
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("compile_tool_decision 需要 tools 参数".to_string()))?;

    if tools.is_empty() {
        return Ok(CompilerResult {
            decision: CompilerDecision::FinalAnswer,
            usage: CompilerUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
            },
        });
    }

    let valid_tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    let openai_tools = convert_anthropic_tools_to_openai(tools);

    let tool_choice = req
        .tool_choice
        .as_ref()
        .map(|tc| convert_anthropic_tool_choice_to_openai(tc, tools))
        .unwrap_or_else(|| OpenAiToolChoice::String("auto".to_string()));

    let model = config
        .model
        .as_ref()
        .ok_or_else(|| AppError::Config("tool_runtime.model 未配置".to_string()))?;

    let base_url = config
        .base_url
        .as_ref()
        .ok_or_else(|| AppError::Config("tool_runtime.base_url 未配置".to_string()))?;

    let api_key = config.get_api_key()?;

    let user_message = build_compiler_user_message(req, ouyi_output);

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

    tracing::debug!(body = %body, "编译器响应原文");

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
            Ok(calls) if !calls.is_empty() => Ok(CompilerResult {
                decision: CompilerDecision::ToolCalls(calls),
                usage,
            }),
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
                    &valid_tool_names,
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

async fn compile_tool_decision_anthropic(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    req: &MessagesRequest,
    ouyi_output: &str,
) -> Result<CompilerResult, AppError> {
    let tools = req
        .tools
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("compile_tool_decision 需要 tools 参数".to_string()))?;

    if tools.is_empty() {
        return Ok(CompilerResult {
            decision: CompilerDecision::FinalAnswer,
            usage: CompilerUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
            },
        });
    }

    let valid_tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
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
    let api_key = config.get_api_key()?;

    let request = AnthropicCompilerRequest {
        model: model.clone(),
        system: "你是工具调用编译器。你的唯一任务是将 Ouyi 主脑输出中的工具意图转换为结构化的 tool_use。如果 Ouyi 输出明确表示需要调用工具，你必须调用对应工具；如果 Ouyi 输出是最终回答或不需要工具，则不要调用工具。不要添加任何额外解释。".to_string(),
        messages: vec![AnthropicCompilerMessage {
            role: "user".to_string(),
            content: build_compiler_user_message(req, ouyi_output),
        }],
        max_tokens: 512,
        tools: tools.clone(),
        tool_choice: convert_anthropic_tool_choice_to_anthropic(req.tool_choice.as_ref()),
    };

    let url = format!("{}/v1/messages", base_url);
    let response = http_client
        .post(&url)
        .header("x-api-key", api_key)
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

    tracing::debug!(body = %body, "Anthropic 编译器响应原文");

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
        Ok(CompilerResult {
            decision: CompilerDecision::ToolCalls(calls),
            usage,
        })
    }
}

/// 结构化重试：使用更明确的提示重新请求编译器
async fn retry_with_structured_prompt(
    http_client: reqwest::Client,
    config: &ToolRuntimeConfig,
    original_request: &OpenAiChatRequest,
    valid_tool_names: &[String],
) -> Result<CompilerResult, AppError> {
    let retry_request = OpenAiChatRequest {
        model: original_request.model.clone(),
        messages: vec![
            OpenAiMessage {
                role: "system".to_string(),
                content: "你是工具调用编译器。你必须严格按照以下格式返回 tool_calls。每个 tool_calls 的 arguments 必须是有效的 JSON object。不要返回任何文本内容。".to_string(),
            },
            OpenAiMessage {
                role: "user".to_string(),
                content: format!(
                    "请重新解析并返回 tool_calls。可用工具：{}。必须确保 arguments 是合法 JSON object。",
                    valid_tool_names.join(", ")
                ),
            },
        ],
        tools: original_request.tools.clone(),
        tool_choice: OpenAiToolChoice::String("auto".to_string()),
    };

    let base_url = config
        .base_url
        .as_ref()
        .ok_or_else(|| AppError::Config("tool_runtime.base_url 未配置".to_string()))?;

    let api_key = config.get_api_key()?;

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = http_client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
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

    tracing::debug!(body = %body, "编译器重试响应原文");

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

        let compiled_calls = parse_openai_tool_calls(tool_calls, valid_tool_names)?;
        if compiled_calls.is_empty() {
            Ok(CompilerResult {
                decision: CompilerDecision::FinalAnswer,
                usage,
            })
        } else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_tool(name: &str) -> AnthropicTool {
        AnthropicTool {
            name: name.to_string(),
            description: "测试工具".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }),
        }
    }

    #[test]
    fn test_convert_anthropic_tools_to_openai() {
        let tools = vec![make_test_tool("Read"), make_test_tool("Write")];
        let openai_tools = convert_anthropic_tools_to_openai(&tools);

        assert_eq!(openai_tools.len(), 2);
        assert_eq!(openai_tools[0].r#type, "function");
        assert_eq!(openai_tools[0].function.name, "Read");
        assert_eq!(openai_tools[1].function.name, "Write");
    }

    #[test]
    fn test_convert_tool_choice_auto() {
        let choice = ToolChoice::Auto;
        let tools = vec![make_test_tool("Read")];
        let openai_choice = convert_anthropic_tool_choice_to_openai(&choice, &tools);
        assert_eq!(serde_json::to_string(&openai_choice).unwrap(), "\"auto\"");
    }

    #[test]
    fn test_convert_tool_choice_any() {
        let choice = ToolChoice::Any;
        let tools = vec![make_test_tool("Read")];
        let openai_choice = convert_anthropic_tool_choice_to_openai(&choice, &tools);
        assert_eq!(
            serde_json::to_string(&openai_choice).unwrap(),
            "\"required\""
        );
    }

    #[test]
    fn test_convert_tool_choice_specific() {
        let choice = ToolChoice::Tool {
            name: "Read".to_string(),
        };
        let tools = vec![make_test_tool("Read")];
        let openai_choice = convert_anthropic_tool_choice_to_openai(&choice, &tools);
        let json = serde_json::to_string(&openai_choice).unwrap();
        assert!(json.contains("function"));
        assert!(json.contains("Read"));
    }

    #[test]
    fn test_convert_anthropic_compiler_tool_choice_specific() {
        let choice = ToolChoice::Tool {
            name: "Read".to_string(),
        };

        match convert_anthropic_tool_choice_to_anthropic(Some(&choice)) {
            AnthropicCompilerToolChoice::Tool { name } => assert_eq!(name, "Read"),
            _ => panic!("expected specific Anthropic tool choice"),
        }
    }

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

    fn make_request_with_tools() -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![crate::model::anthropic::AnthropicMessage {
                role: "user".to_string(),
                content: crate::model::anthropic::AnthropicContent::Text("test".to_string()),
            }],
            system: None,
            max_tokens: 1024,
            stream: Some(true),
            tools: Some(vec![make_test_tool("Bash")]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        }
    }

    #[test]
    fn test_compiler_user_message_requires_tool_json_only() {
        let req = make_request_with_tools();
        let message = build_compiler_user_message(&req, "需要执行 pwd 查看当前目录。");
        assert!(message.contains("只输出工具调用"));
        assert!(message.contains("不要输出最终回答"));
        assert!(!message.contains("工具详细信息列表"));
    }
}
