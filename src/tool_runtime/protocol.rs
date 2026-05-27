use crate::model::anthropic::AnthropicTool;
use crate::model::openai::ToolDef;

/// 协议无关工具规格，用于 OpenAI 和 Anthropic 工具的统一表示
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// 工具端点类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEndpointKind {
    OpenAi,
    #[allow(dead_code)]
    Anthropic,
}

/// 工具选择规格
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoiceSpec {
    Auto,
    Required,
    Tool(String),
}

/// 工具运行时请求上下文，用于抽象不同端点的工具编译
#[derive(Debug, Clone)]
pub struct ToolRuntimeRequestContext {
    pub endpoint: ToolEndpointKind,
    #[allow(dead_code)]
    pub model: String,
    pub latest_user_text: Option<String>,
    pub latest_tool_result: Option<String>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<ToolChoiceSpec>,
    pub payloads: Vec<crate::tool_runtime::payload::ToolPayload>,
}

/// 将 OpenAI 工具定义转换为协议无关规格
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

/// 将 Anthropic 工具定义转换为协议无关规格
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

/// 将 OpenAI ChatCompletionRequest 转换为工具运行时上下文
pub fn openai_request_to_context(
    req: &crate::model::openai::ChatCompletionRequest,
    tools: Vec<ToolSpec>,
) -> ToolRuntimeRequestContext {
    let latest_user_text = req
        .messages
        .iter()
        .rev()
        .find(|msg| msg.role == "user")
        .and_then(|msg| msg.content.clone());

    let latest_tool_result = req
        .messages
        .iter()
        .rev()
        .find(|msg| msg.role == "tool")
        .and_then(|msg| msg.content.clone());

    let tool_choice = req.tool_choice.as_ref().and_then(|tc| {
        if let Some(str_val) = tc.as_str() {
            match str_val {
                "auto" => Some(ToolChoiceSpec::Auto),
                "required" => Some(ToolChoiceSpec::Required),
                _ => Some(ToolChoiceSpec::Auto),
            }
        } else if let Some(obj) = tc.as_object() {
            if let Some(func) = obj.get("function") {
                if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                    Some(ToolChoiceSpec::Tool(name.to_string()))
                } else {
                    Some(ToolChoiceSpec::Auto)
                }
            } else {
                Some(ToolChoiceSpec::Auto)
            }
        } else {
            Some(ToolChoiceSpec::Auto)
        }
    });

    ToolRuntimeRequestContext {
        endpoint: ToolEndpointKind::OpenAi,
        model: req.model.clone(),
        latest_user_text,
        latest_tool_result,
        tools,
        tool_choice,
        payloads: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let ctx = openai_request_to_context(&req, Vec::new());
        assert_eq!(ctx.endpoint, ToolEndpointKind::OpenAi);
        assert_eq!(ctx.model, "gpt-test");
        assert_eq!(ctx.latest_user_text.as_deref(), Some("hello"));
        assert!(ctx.latest_tool_result.is_none());
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

        let ctx = openai_request_to_context(&req, Vec::new());
        assert_eq!(ctx.endpoint, ToolEndpointKind::OpenAi);
        assert_eq!(ctx.model, "gpt-test");
        assert_eq!(ctx.latest_tool_result.as_deref(), Some("PENDING"));
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

        let ctx = openai_request_to_context(&req, Vec::new());
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

        let ctx = openai_request_to_context(&req, Vec::new());
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

        let ctx = openai_request_to_context(&req, Vec::new());
        assert_eq!(
            ctx.tool_choice,
            Some(ToolChoiceSpec::Tool("Read".to_string()))
        );
    }

    #[test]
    fn test_context_accepts_payload_metadata() {
        let mut ctx = ToolRuntimeRequestContext {
            endpoint: ToolEndpointKind::OpenAi,
            model: "claude-test".to_string(),
            latest_user_text: Some("write report".to_string()),
            latest_tool_result: None,
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
}
