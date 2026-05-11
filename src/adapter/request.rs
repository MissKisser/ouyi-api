use crate::model::openai::ChatCompletionRequest;

const TOOL_PROMPT_TEMPLATE: &str = r#"

You have access to the following tools. To use a tool, wrap your tool call in <tool_use> tags as shown below:

<tool_use>
{"name": "tool_name", "arguments": {"param1": "value1"}}
</tool_use>

Rules:
1. You may make multiple tool calls by using multiple <tool_use> blocks.
2. Each <tool_use> block must contain valid JSON with "name" and "arguments" fields.
3. The "arguments" must be a JSON object matching the tool's parameter schema.
4. Do NOT wrap tool calls in code blocks or any other formatting.
5. If you do not need to use any tools, respond normally without <tool_use> blocks.
6. You can mix regular text with tool calls in your response.
"#;

pub struct OuyiRequest {
    pub system_prompt: Option<String>,
    pub text: String,
    pub thinking: bool,
}

pub fn convert_request(
    openai_req: &ChatCompletionRequest,
    model_full: &str,
    should_thinking: bool,
) -> Result<OuyiRequest, String> {
    let messages = &openai_req.messages;
    if messages.is_empty() {
        return Err("messages不能为空".to_string());
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    for msg in messages {
        let content = match &msg.content {
            Some(c) => c.clone(),
            None => {
                if msg.tool_calls.is_some() {
                    continue;
                }
                String::new()
            }
        };

        match msg.role.as_str() {
            "system" => {
                system_parts.push(content);
            }
            "user" => {
                text_parts.push(format!("[User]: {}", content));
            }
            "assistant" => {
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        text_parts.push(format!(
                            "[Assistant]: <tool_use>\n{}\n</tool_use>",
                            serde_json::json!({
                                "name": tc.function.name,
                                "arguments": args,
                            })
                        ));
                    }
                    if !content.is_empty() {
                        text_parts.push(format!("[Assistant]: {}", content));
                    }
                } else {
                    text_parts.push(format!("[Assistant]: {}", content));
                }
            }
            "tool" => {
                let tool_name = msg.name.as_deref().unwrap_or("unknown");
                text_parts.push(format!(
                    "<tool_result name=\"{}\">{}</tool_result>",
                    tool_name, content
                ));
            }
            _ => {
                text_parts.push(format!("[{}]: {}", msg.role, content));
            }
        }
    }

    let has_tools = openai_req.tools.is_some();
    if has_tools {
        let tool_descriptions = build_tool_descriptions(openai_req);
        system_parts.push(format!(
            "{}\n\nAvailable tools:\n{}",
            TOOL_PROMPT_TEMPLATE, tool_descriptions
        ));
    }

    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let text = text_parts.join("\n\n");
    let thinking = should_thinking || model_full.contains("-thinking");

    Ok(OuyiRequest {
        system_prompt,
        text,
        thinking,
    })
}

fn build_tool_descriptions(req: &ChatCompletionRequest) -> String {
    let tools = match &req.tools {
        Some(t) => t,
        None => return String::new(),
    };

    tools
        .iter()
        .map(|t| {
            let desc = t
                .function
                .description
                .as_deref()
                .unwrap_or("No description");
            let params = t
                .function
                .parameters
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "{}".to_string()))
                .unwrap_or_else(|| "{}".to_string());
            format!(
                "- {}: {}\n  Parameters: {}",
                t.function.name, desc, params
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::openai::{ChatMessage, ToolDef, ToolFunction};

    fn make_simple_request(role: &str, content: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test".to_string(),
            messages: vec![ChatMessage {
                role: role.to_string(),
                content: Some(content.to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
        }
    }

    #[test]
    fn test_basic_user_message() {
        let req = make_simple_request("user", "hello");
        let result = convert_request(&req, "claude_chat::test", false).unwrap();
        assert!(result.text.contains("[User]: hello"));
        assert!(result.system_prompt.is_none());
        assert!(!result.thinking);
    }

    #[test]
    fn test_system_prompt_extraction() {
        let req = ChatCompletionRequest {
            model: "test".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: Some("You are helpful".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: Some("hi".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
        };
        let result = convert_request(&req, "test", false).unwrap();
        assert_eq!(result.system_prompt.as_deref(), Some("You are helpful"));
        assert!(result.text.contains("[User]: hi"));
        assert!(!result.text.contains("You are helpful"));
    }

    #[test]
    fn test_thinking_detection() {
        let req = make_simple_request("user", "hello");
        let result =
            convert_request(&req, "deepseek_chat::deepseek-r1-thinking", false).unwrap();
        assert!(result.thinking);
    }

    #[test]
    fn test_tool_result_message() {
        let req = ChatCompletionRequest {
            model: "test".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: Some("read file".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some("file content here".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("call_123".to_string()),
                    name: Some("read_file".to_string()),
                },
            ],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
        };
        let result = convert_request(&req, "test", false).unwrap();
        assert!(result
            .text
            .contains(r#"<tool_result name="read_file">file content here</tool_result>"#));
    }

    #[test]
    fn test_tool_injection() {
        let req = ChatCompletionRequest {
            model: "test".to_string(),
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
            tools: Some(vec![ToolDef {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    description: Some("Read a file".to_string()),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {"path": {"type": "string"}}
                    })),
                },
            }]),
        };
        let result = convert_request(&req, "test", false).unwrap();
        let sys = result.system_prompt.unwrap();
        assert!(sys.contains("<tool_use>"));
        assert!(sys.contains("read_file"));
    }

    #[test]
    fn test_empty_messages_rejected() {
        let req = ChatCompletionRequest {
            model: "test".to_string(),
            messages: vec![],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
        };
        assert!(convert_request(&req, "test", false).is_err());
    }
}
