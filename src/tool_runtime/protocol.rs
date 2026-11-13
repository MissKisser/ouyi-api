//! 协议无关的工具规格层。
//! 把 OpenAI 与 Anthropic 两种协议的工具定义、tool_choice 统一为内部规格
//! （`ToolSpec` / `ToolChoiceSpec`），并在此层完成工具启用判定、tool_choice
//! 约束校验与调用 ID 归一化，供 broker / 决策层与编译器共用。

use crate::model::anthropic::AnthropicTool;
use crate::model::openai::ToolDef;

/// 判断 OpenAI Chat 请求是否启用了工具调用。
///
/// 条件：携带非空工具列表，且 `tool_choice` 不是字符串 `"none"`。
///
/// # 参数
/// - `req`：OpenAI Chat 请求。
///
/// # 返回
/// 工具调用启用时为 `true`。
pub fn openai_tools_enabled(req: &crate::model::openai::ChatCompletionRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        && req.tool_choice.as_ref().and_then(|choice| choice.as_str()) != Some("none")
}

/// 协议无关工具规格，用于 OpenAI 和 Anthropic 工具的统一表示。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// 工具名称。
    pub name: String,
    /// 工具功能描述；OpenAI 侧缺省时为空串。
    pub description: String,
    /// 工具入参的 JSON Schema；OpenAI 侧缺省时为空对象 schema。
    pub input_schema: serde_json::Value,
}

/// 工具端点类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEndpointKind {
    /// OpenAI 协议端点。
    OpenAi,
    /// Anthropic 协议端点（当前流程未消费，保留供统一抽象）。
    #[allow(dead_code)]
    Anthropic,
}

/// 工具选择规格（协议无关的 tool_choice 表示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoiceSpec {
    /// 由模型自行决定是否调用工具。
    Auto,
    /// 强制必须产生工具调用。
    Required,
    /// 强制调用指定名称的工具。
    Tool(String),
}

/// 把 Anthropic 的 ToolChoice 转换为协议无关规格。
///
/// # 参数
/// - `choice`：Anthropic 请求中的 tool_choice；`None` 表示未设置。
///
/// # 返回
/// 对应规格；未设置时为 `None`（auto 映射为 `Auto`，any 映射为 `Required`）。
pub fn anthropic_tool_choice_to_spec(
    choice: Option<&crate::model::anthropic::ToolChoice>,
) -> Option<ToolChoiceSpec> {
    match choice {
        Some(crate::model::anthropic::ToolChoice::Auto) => Some(ToolChoiceSpec::Auto),
        Some(crate::model::anthropic::ToolChoice::Any) => Some(ToolChoiceSpec::Required),
        Some(crate::model::anthropic::ToolChoice::Tool { name }) => {
            Some(ToolChoiceSpec::Tool(name.clone()))
        }
        None => None,
    }
}

/// 把 OpenAI 侧的 tool_choice JSON 转换为协议无关规格。
///
/// 字符串形式：`"auto"` / `"required"` / 其它值一律按 `Auto`；
/// 对象形式：取 `function.name` 或顶层 `name` 映射为 `Tool`，取不到名称
/// 时按 `Auto` 处理；其它类型按 `Auto` 处理。`None` 输入返回 `None`。
///
/// # 参数
/// - `choice`：请求中的 tool_choice 原始 JSON。
///
/// # 返回
/// 对应规格；输入为 `None` 时为 `None`。
pub fn openai_tool_choice_to_spec(choice: Option<&serde_json::Value>) -> Option<ToolChoiceSpec> {
    choice.and_then(|tool_choice| {
        if let Some(value) = tool_choice.as_str() {
            match value {
                "auto" => Some(ToolChoiceSpec::Auto),
                "required" => Some(ToolChoiceSpec::Required),
                _ => Some(ToolChoiceSpec::Auto),
            }
        } else if let Some(object) = tool_choice.as_object() {
            object
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| object.get("name"))
                .and_then(|name| name.as_str())
                .map(|name| ToolChoiceSpec::Tool(name.to_string()))
                .or(Some(ToolChoiceSpec::Auto))
        } else {
            Some(ToolChoiceSpec::Auto)
        }
    })
}

/// 校验实际产生的工具调用是否满足 tool_choice 约束。
///
/// `Required` 要求至少有一个调用；`Tool(name)` 要求全部调用均为该工具。
/// `Auto` 或未设置时不做约束。
///
/// # 参数
/// - `choice`：请求的 tool_choice 规格。
/// - `actual_names`：实际产生的工具调用名称列表（为空表示无调用）。
///
/// # 返回
/// 约束满足返回 `Ok(())`。
///
/// # 错误
/// 约束不满足时返回 `AppError::ToolProtocol`，消息含
/// `expected=<约束>` 与 `actual=<实际调用>`。
pub fn validate_tool_choice(
    choice: Option<&ToolChoiceSpec>,
    actual_names: &[String],
) -> Result<(), crate::error::AppError> {
    let expected = match choice {
        Some(ToolChoiceSpec::Required) if actual_names.is_empty() => Some("required".to_string()),
        Some(ToolChoiceSpec::Tool(name))
            if actual_names.is_empty() || actual_names.iter().any(|actual| actual != name) =>
        {
            Some(name.clone())
        }
        _ => None,
    };

    if let Some(expected) = expected {
        let actual = if actual_names.is_empty() {
            "<none>".to_string()
        } else {
            actual_names.join(",")
        };
        return Err(crate::error::AppError::ToolProtocol(format!(
            "工具选择约束不满足: expected={expected}, actual={actual}"
        )));
    }

    Ok(())
}

/// 将工具调用 ID 就地重写为 `call_proxy_<uuid>` 形式。
///
/// 上游可能输出重复或不唯一的调用 ID，统一重写可保证 ID 全局唯一，
/// 避免账本与会话记录互相串扰。
///
/// # 参数
/// - `ids`：待重写的调用 ID 可变引用迭代。
pub fn normalize_tool_call_ids<'a>(ids: impl IntoIterator<Item = &'a mut String>) {
    for id in ids {
        *id = format!("call_proxy_{}", uuid::Uuid::new_v4().simple());
    }
}

/// 工具运行时请求上下文，用于抽象不同端点的工具编译。
#[derive(Debug, Clone)]
pub struct ToolRuntimeRequestContext {
    /// 请求来源端点类型。
    pub endpoint: ToolEndpointKind,
    /// 请求的模型标识（当前流程未消费，保留供统一抽象）。
    #[allow(dead_code)]
    pub model: String,
    /// 规范化后的会话时间线。
    pub transcript: Vec<crate::tool_runtime::context::TranscriptItem>,
    /// 可用工具规格列表。
    pub tools: Vec<ToolSpec>,
    /// 工具选择规格。
    pub tool_choice: Option<ToolChoiceSpec>,
    /// 主模型输出中抽离的长文本负载。
    pub payloads: Vec<crate::tool_runtime::payload::ToolPayload>,
}

/// 将 OpenAI 工具定义转换为协议无关规格。
///
/// 仅保留 `type == "function"` 的定义；description / parameters 缺省时
/// 分别取空串与空对象 schema。
///
/// # 参数
/// - `tools`：OpenAI 工具定义列表。
///
/// # 返回
/// 转换后的工具规格列表，顺序与输入一致。
pub fn openai_tools_to_specs(tools: &[ToolDef]) -> Vec<ToolSpec> {
    tools
        .iter()
        .filter(|tool| tool.tool_type == "function")
        .map(|tool| ToolSpec {
            name: tool.function.name.clone(),
            description: tool.function.description.clone().unwrap_or_default(),
            input_schema: tool
                .function
                .parameters
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        })
        .collect()
}

/// 将 Anthropic 工具定义转换为协议无关规格。
///
/// # 参数
/// - `tools`：Anthropic 工具定义列表。
///
/// # 返回
/// 转换后的工具规格列表，字段一一对应，顺序与输入一致。
pub fn anthropic_tools_to_specs(tools: &[AnthropicTool]) -> Vec<ToolSpec> {
    tools
        .iter()
        .map(|tool| ToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        })
        .collect()
}

/// 将 OpenAI Chat 请求转换为工具运行时上下文。
///
/// 消息历史经 context 模块规范化为时间线，tool_choice 转为规格表示，
/// 负载列表初始为空（由调用方在提取 payload 后填充）。
///
/// # 参数
/// - `req`：OpenAI Chat 请求。
/// - `tools`：已转换的工具规格列表。
///
/// # 返回
/// 端点标记为 OpenAi 的工具运行时上下文。
///
/// # 错误
/// 消息历史不规范（工具结果缺 ID、调用参数 JSON 非法等）时返回
/// `AppError::BadRequest`。
pub fn openai_request_to_context(
    req: &crate::model::openai::ChatCompletionRequest,
    tools: Vec<ToolSpec>,
) -> Result<ToolRuntimeRequestContext, crate::error::AppError> {
    let transcript = crate::tool_runtime::context::from_openai_chat(req)
        .map_err(crate::error::AppError::BadRequest)?;

    let tool_choice = openai_tool_choice_to_spec(req.tool_choice.as_ref());

    Ok(ToolRuntimeRequestContext {
        endpoint: ToolEndpointKind::OpenAi,
        model: req.model.clone(),
        transcript,
        tools,
        tool_choice,
        payloads: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::anthropic::ToolChoice as AnthropicToolChoice;
    use crate::model::openai::{ChatCompletionRequest, ChatMessage, ToolFunction};

    #[test]
    fn test_openai_tool_to_spec() {
        let tools = vec![ToolDef {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "Read".to_string(),
                description: Some("Read file".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string"}
                    },
                    "required": ["file_path"]
                })),
            },
        }];

        let specs = openai_tools_to_specs(&tools);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "Read");
        assert_eq!(specs[0].description, "Read file");
        assert_eq!(specs[0].input_schema["type"], "object");
    }

    #[test]
    fn test_openai_tools_to_specs_filters_non_function() {
        let tools = vec![
            ToolDef {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "Read".to_string(),
                    description: Some("Read file".to_string()),
                    parameters: Some(serde_json::json!({"type": "object"})),
                },
            },
            ToolDef {
                tool_type: "code_execution".to_string(),
                function: ToolFunction {
                    name: "ignored".to_string(),
                    description: None,
                    parameters: None,
                },
            },
        ];

        let specs = openai_tools_to_specs(&tools);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "Read");
    }

    #[test]
    fn test_openai_tool_without_description_or_parameters() {
        let tools = vec![ToolDef {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "Bash".to_string(),
                description: None,
                parameters: None,
            },
        }];

        let specs = openai_tools_to_specs(&tools);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "Bash");
        assert_eq!(specs[0].description, "");
        assert_eq!(specs[0].input_schema["type"], "object");
    }

    #[test]
    fn test_anthropic_tool_to_spec() {
        let tools = vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "Run shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        }];

        let specs = anthropic_tools_to_specs(&tools);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "Bash");
        assert_eq!(specs[0].description, "Run shell command");
        assert_eq!(
            specs[0].input_schema["properties"]["command"]["type"],
            "string"
        );
    }

    #[test]
    fn test_openai_request_to_context_basic() {
        let req = ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        let ctx = openai_request_to_context(&req, Vec::new()).unwrap();
        assert_eq!(ctx.endpoint, ToolEndpointKind::OpenAi);
        assert_eq!(ctx.model, "gpt-test");
        assert!(matches!(
            &ctx.transcript[0],
            crate::tool_runtime::context::TranscriptItem::UserText { content }
                if content == "hello"
        ));
    }

    #[test]
    fn test_openai_context_keeps_latest_tool_result() {
        let req = ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: Some("run command".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some("PENDING".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: Some("Bash".to_string()),
                },
            ],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        let ctx = openai_request_to_context(&req, Vec::new()).unwrap();
        assert_eq!(ctx.endpoint, ToolEndpointKind::OpenAi);
        assert_eq!(ctx.model, "gpt-test");
        assert!(matches!(
            &ctx.transcript[1],
            crate::tool_runtime::context::TranscriptItem::ToolResult { output, .. }
                if output == "PENDING"
        ));
    }

    #[test]
    fn test_openai_context_tool_choice_auto() {
        let req = ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("test".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: Some(serde_json::json!("auto")),
            parallel_tool_calls: None,
        };

        let ctx = openai_request_to_context(&req, Vec::new()).unwrap();
        assert_eq!(ctx.tool_choice, Some(ToolChoiceSpec::Auto));
    }

    #[test]
    fn test_openai_context_tool_choice_required() {
        let req = ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("test".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: Some(serde_json::json!("required")),
            parallel_tool_calls: None,
        };

        let ctx = openai_request_to_context(&req, Vec::new()).unwrap();
        assert_eq!(ctx.tool_choice, Some(ToolChoiceSpec::Required));
    }

    #[test]
    fn test_openai_context_tool_choice_specific() {
        let req = ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("test".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: Some(serde_json::json!({
                "type": "function",
                "function": {"name": "Read"}
            })),
            parallel_tool_calls: None,
        };

        let ctx = openai_request_to_context(&req, Vec::new()).unwrap();
        assert_eq!(
            ctx.tool_choice,
            Some(ToolChoiceSpec::Tool("Read".to_string()))
        );
    }

    #[test]
    fn test_openai_tools_enabled_respects_none_choice() {
        let mut req = ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("test".to_string()),
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
                    description: None,
                    parameters: None,
                },
            }]),
            tool_choice: None,
            parallel_tool_calls: None,
        };
        req.tool_choice = Some(serde_json::json!("none"));
        assert!(!openai_tools_enabled(&req));

        req.tool_choice = Some(serde_json::json!("auto"));
        assert!(openai_tools_enabled(&req));
    }

    #[test]
    fn test_context_accepts_payload_metadata() {
        let mut ctx = ToolRuntimeRequestContext {
            endpoint: ToolEndpointKind::OpenAi,
            model: "claude-test".to_string(),
            transcript: vec![crate::tool_runtime::context::TranscriptItem::UserText {
                content: "write report".to_string(),
            }],
            tool_choice: None,
            tools: vec![],
            payloads: vec![crate::tool_runtime::payload::ToolPayload {
                id: "p1".to_string(),
                body: "report".to_string(),
                byte_len: 6,
            }],
        };

        assert_eq!(ctx.payloads[0].id, "p1");
        ctx.payloads.clear();
        assert!(ctx.payloads.is_empty());
    }

    #[test]
    fn test_anthropic_tool_choice_maps_to_runtime_spec() {
        assert_eq!(
            anthropic_tool_choice_to_spec(Some(&AnthropicToolChoice::Any)),
            Some(ToolChoiceSpec::Required)
        );
        assert_eq!(
            anthropic_tool_choice_to_spec(Some(&AnthropicToolChoice::Tool {
                name: "Read".to_string(),
            })),
            Some(ToolChoiceSpec::Tool("Read".to_string()))
        );
        assert_eq!(
            anthropic_tool_choice_to_spec(Some(&AnthropicToolChoice::Auto)),
            Some(ToolChoiceSpec::Auto)
        );
    }

    #[test]
    fn test_responses_style_function_choice_maps_to_named_tool() {
        let choice = serde_json::json!({"type": "function", "name": "Read"});
        assert_eq!(
            openai_tool_choice_to_spec(Some(&choice)),
            Some(ToolChoiceSpec::Tool("Read".to_string()))
        );
    }

    #[test]
    fn test_forced_tool_choice_rejects_missing_or_wrong_calls() {
        let err = validate_tool_choice(Some(&ToolChoiceSpec::Required), &[]).unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::ToolProtocol(message) if message.contains("expected=required") && message.contains("actual=<none>"))
        );

        let err = validate_tool_choice(
            Some(&ToolChoiceSpec::Tool("Read".to_string())),
            &["Bash".to_string()],
        )
        .unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::ToolProtocol(message) if message.contains("expected=Read") && message.contains("actual=Bash"))
        );
    }

    #[test]
    fn test_normalize_provider_call_ids_discards_duplicate_provider_ids() {
        let mut ids = ["call_0".to_string(), "call_0".to_string()];
        normalize_tool_call_ids(ids.iter_mut());
        assert_ne!(ids[0], ids[1]);
        assert!(ids.iter().all(|id| id.starts_with("call_proxy_")));
    }
}
