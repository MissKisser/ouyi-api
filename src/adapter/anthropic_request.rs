use crate::model::anthropic::{
    AnthropicContent, AnthropicTool, ContentBlock, MessagesRequest, SystemContent,
};

pub struct OuyiRequest {
    pub system_prompt: Option<String>,
    pub text: String,
    pub thinking: bool,
}

const TOOL_PROMPT_ENHANCED: &str = r#"
## 工具调用规范

你可以使用以下工具完成任务。每次使用时，严格按以下格式输出（不要加 markdown 代码块）：

<tool_use>
{"name": "工具名", "arguments": {"参数名": "值"}}
</tool_use>

规则：
1. 每个 <tool_use> 块只能包含一个工具调用
2. arguments 必须是有效的 JSON 对象
3. 不要使用代码块包裹 <tool_use>
4. 工具执行后，结果以 <tool_result> 格式返回，你必须读取并继续任务
5. 如果不需要工具，直接回复即可
"#;

pub fn convert_anthropic_request(
    req: &MessagesRequest,
    model_full: &str,
    should_thinking: bool,
) -> Result<OuyiRequest, String> {
    if req.messages.is_empty() {
        return Err("messages不能为空".to_string());
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // 工具指令前置（在 system prompt 之前）
    if let Some(tools) = &req.tools {
        if !tools.is_empty() {
            let tool_descriptions = build_compact_tool_descriptions(tools);
            system_parts.push(format!(
                "{}\n\n可用工具：\n{}",
                TOOL_PROMPT_ENHANCED, tool_descriptions
            ));
        }
    }

    if let Some(sys) = &req.system {
        let sys_text = match sys {
            SystemContent::Text(t) => t.clone(),
            SystemContent::Blocks(blocks) => blocks
                .iter()
                .filter(|b| b.r#type == "text")
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        if !sys_text.is_empty() {
            system_parts.push(sys_text);
        }
    }

    let mut last_role: Option<&str> = None;

    for msg in &req.messages {
        let role_changed = last_role != Some(msg.role.as_str());
        if role_changed && !text_parts.is_empty() {
            text_parts.push(String::new());
        }
        last_role = Some(msg.role.as_str());

        match &msg.content {
            AnthropicContent::Text(content) => match msg.role.as_str() {
                "user" => text_parts.push(format!("[User]: {}", content)),
                "assistant" => text_parts.push(format!("[Assistant]: {}", content)),
                _ => text_parts.push(format!("[{}]: {}", msg.role, content)),
            },
            AnthropicContent::Blocks(blocks) => {
                let mut role_texts: Vec<String> = Vec::new();
                let role_prefix = match msg.role.as_str() {
                    "user" => "[User]",
                    "assistant" => "[Assistant]",
                    other => other,
                };
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                role_texts.push(format!("{}: {}", role_prefix, text));
                            }
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            if !thinking.is_empty() {
                                role_texts.push(format!(
                                    "{}: <think>\n{}\n</think>",
                                    role_prefix, thinking
                                ));
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_id_to_name.insert(id.clone(), name.clone());
                            let tool_json = serde_json::json!({
                                "name": name,
                                "arguments": input,
                            });
                            role_texts.push(format!(
                                "{}: <tool_use>\n{}\n</tool_use>",
                                role_prefix,
                                serde_json::to_string(&tool_json).unwrap_or_default()
                            ));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        } => {
                            let tool_name = tool_id_to_name
                                .get(tool_use_id)
                                .map(|s| s.as_str())
                                .unwrap_or(tool_use_id);
                            let display_content = if content.len() > 2000 {
                                format!(
                                    "{}\n\n[...内容截断，共 {} 字符...]",
                                    &content[..2000],
                                    content.len()
                                )
                            } else {
                                content.clone()
                            };
                            role_texts.push(format!(
                                "[Tool Result for {}]:\n<tool_result name=\"{}\">\n{}\n</tool_result>\n[End of tool result for {}. Continue.]",
                                tool_name, tool_name, display_content, tool_name
                            ));
                        }
                    }
                }
                if !role_texts.is_empty() {
                    text_parts.push(role_texts.join("\n"));
                }
            }
        }
    }

    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let text = text_parts.join("\n");
    let thinking = should_thinking || model_full.contains("-thinking");

    Ok(OuyiRequest {
        system_prompt,
        text,
        thinking,
    })
}

fn build_compact_tool_descriptions(tools: &[AnthropicTool]) -> String {
    tools
        .iter()
        .map(|t| {
            let params = summarize_input_schema(&t.input_schema);
            format!("- {}: {}\n  参数: {}", t.name, t.description, params)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarize_input_schema(schema: &serde_json::Value) -> String {
    let props = schema.get("properties").and_then(|p| p.as_object());
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    match props {
        Some(p) if !p.is_empty() => {
            let fields: Vec<String> = p
                .keys()
                .map(|k| {
                    let desc = p[k]
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    let short_desc: String = desc
                        .chars()
                        .take(40)
                        .collect();
                    if required.contains(&k.as_str()) {
                        format!("{}*", k)
                    } else if short_desc.len() < desc.len() {
                        format!("{}: {}...", k, short_desc)
                    } else {
                        format!("{}: {}", k, short_desc)
                    }
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        _ => "{}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::anthropic::{AnthropicContent, AnthropicMessage, ContentBlock};

    fn make_req(content: AnthropicContent) -> MessagesRequest {
        MessagesRequest {
            model: "test".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content,
            }],
            system: None,
            max_tokens: 100,
            stream: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
        }
    }

    #[test]
    fn test_basic_message() {
        let req = make_req(AnthropicContent::Text("Hello".to_string()));
        let result = convert_anthropic_request(&req, "test", false).unwrap();
        assert!(result.text.contains("[User]: Hello"));
    }

    #[test]
    fn test_system_prompt() {
        let mut req = make_req(AnthropicContent::Text("hi".to_string()));
        req.system = Some(SystemContent::Text("You are helpful".to_string()));
        let result = convert_anthropic_request(&req, "test", false).unwrap();
        assert_eq!(result.system_prompt.as_deref(), Some("You are helpful"));
    }

    #[test]
    fn test_tool_use_message() {
        let req = MessagesRequest {
            model: "test".to_string(),
            messages: vec![AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::Text {
                        text: "Let me check".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "tool_1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "/tmp/test"}),
                    },
                ]),
            }],
            system: None,
            max_tokens: 100,
            stream: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
        };
        let result = convert_anthropic_request(&req, "test", false).unwrap();
        assert!(result.text.contains("<tool_use>"));
        assert!(result.text.contains("read_file"));
    }

    #[test]
    fn test_empty_messages_rejected() {
        let req = MessagesRequest {
            model: "test".to_string(),
            messages: vec![],
            system: None,
            max_tokens: 100,
            stream: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
        };
        assert!(convert_anthropic_request(&req, "test", false).is_err());
    }

    #[test]
    fn test_tool_descriptions_compact() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in ms"
                }
            },
            "required": ["command"]
        });
        let summary = summarize_input_schema(&schema);
        assert!(summary.contains("command*"));
        assert!(summary.contains("timeout"));
    }

    #[test]
    fn test_tool_instructions_before_system() {
        let tools = vec![AnthropicTool {
            name: "bash".to_string(),
            description: "Execute a command".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let mut req = make_req(AnthropicContent::Text("hi".to_string()));
        req.system = Some(SystemContent::Text("You are helpful".to_string()));
        req.tools = Some(tools);
        let result = convert_anthropic_request(&req, "test", false).unwrap();
        let sys = result.system_prompt.unwrap();
        let tool_pos = sys.find("工具调用规范").unwrap();
        let system_pos = sys.find("You are helpful").unwrap();
        assert!(tool_pos < system_pos, "工具指令应在系统提示之前");
    }
}