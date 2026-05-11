use crate::model::openai::ChatCompletionRequest;

const TOOL_CALL_FORMAT: &str = concat!(
    "## 工具调用（必须严格遵守）\n\n",
    "当用户的任务需要使用工具才能完成时，你必须调用工具。禁止猜测你不知道的信息、直接回答需要实时数据的问题、或假装已有答案。\n\n",
    "### 格式\n",
    "```text\n",
    "<|tool▁calls▁begin|>\n",
    "[{\"name\": \"工具名\", \"arguments\": {\"参数名\": \"值\"}}]\n",
    "<|tool▁calls▁end|>\n",
    "```\n\n",
    "单个工具用单个 JSON 对象，多个工具用 JSON 数组。\n\n",
    "### 规则\n",
    "1. 标记之间只能包含 JSON 对象或 JSON 数组\n",
    "2. 单次回复只能有一个标记块，多工具以数组形式放在同一块内\n",
    "3. 标记之前你可以先思考要调用哪个工具以及参数（<think>...</think>），然后直接输出工具调用标记\n",
    "4. 不能包裹在 markdown 代码块（```json 等）中——标记内的 JSON 必须裸写\n",
    "5. JSON 必须包含 \"name\"（字符串）和 \"arguments\"（JSON 对象）字段\n",
    "6. arguments 键名必须用双引号，值必须是合法 JSON 类型\n",
    "7. 不允许注释或尾部逗号\n",
    "8. 工具调用之后不要附带任何解释文字\n",
    "9. 如果你确信不需要调用工具即可完整回答，正常回复即可（但涉及文件系统、网络、外部命令时必须调用工具）\n",
    "10. 永远不要编造 <tool_result> 标签——工具结果由系统自动注入\n",
    "11. arguments 的值必须是纯文本，禁止 markdown/LaTeX\n",
);

const TOOL_CALL_REMINDER: &str = concat!(
    "\n\n【工具调用提醒】如果你无法仅凭已有知识完成当前任务，请思考需要调用哪个工具，然后直接输出工具调用标记。不要犹豫，直接行动。",
);

pub struct OuyiRequest {
    pub system_prompt: Option<String>,
    pub text: String,
    pub thinking: bool,
}

fn build_tool_instruction_text(req: &ChatCompletionRequest) -> String {
    let tools = match &req.tools {
        Some(t) => t,
        None => return String::new(),
    };

    if tools.is_empty() {
        return String::new();
    }

    let defs = tools
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
                .map(|p| {
                    let props = p
                        .get("properties")
                        .and_then(|v| v.as_object())
                        .map(|obj| {
                            obj.iter()
                                .map(|(k, v)| {
                                    let desc = v
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("");
                                    format!("      - **{}**: {}", k, desc)
                                })
                                .collect::<Vec<_>>()
                                .join(",\n")
                        })
                        .unwrap_or_default();
                    let required: Vec<&str> = p
                        .get("required")
                        .and_then(|r| r.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    let required_str = if required.is_empty() {
                        String::new()
                    } else {
                        format!(" (必填: {})", required.join(", "))
                    };
                    format!("参数:{{{} }}{}", props, required_str)
                })
                .unwrap_or_else(|| "参数: {}".to_string());

            format!(
                "- **{}** (function)\n  - 说明: {}\n  - {}",
                t.function.name, desc, params
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "{}\n\n### 可用工具\n\n{}\n{}",
        TOOL_CALL_FORMAT, defs, TOOL_CALL_REMINDER
    )
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
                    let calls: Vec<serde_json::Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            serde_json::json!({
                                "name": tc.function.name,
                                "arguments": args,
                            })
                        })
                        .collect();
                    text_parts.push(format!(
                        "[Assistant]: <|tool\u{2581}calls\u{2581}begin|>\n{}\n<|tool\u{2581}calls\u{2581}end|>",
                        serde_json::to_string(&calls).unwrap_or_default()
                    ));
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
        let tool_text = build_tool_instruction_text(openai_req);
        if !tool_text.is_empty() {
            system_parts.push(tool_text);
        }
    }

    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let mut text = text_parts.join("\n\n");

    if has_tools {
        text.push_str("\n\n<think>\n现在请判断是否需要调用工具。如需调用，请输出：\n\n<|tool\u{2581}calls\u{2581}begin|>\n[{\"name\": \"工具名\", \"arguments\": {\"参数名\": \"值\"}}]\n<|tool\u{2581}calls\u{2581}end|>\n\n请立即决定：需要调用工具吗？需要则直接输出上述标记，不需要则正常回复。");
    }

    let thinking = should_thinking || model_full.contains("-thinking");

    Ok(OuyiRequest {
        system_prompt,
        text,
        thinking,
    })
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
        assert!(sys.contains("tool\u{2581}calls\u{2581}begin"));
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
