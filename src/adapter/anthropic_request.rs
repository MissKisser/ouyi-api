use crate::model::anthropic::{
    AnthropicContent, AnthropicTool, ContentBlock, MessagesRequest, SystemContent,
};

pub struct OuyiRequest {
    pub system_prompt: Option<String>,
    pub text: String,
    pub thinking: bool,
}

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

fn build_tool_instruction_block(tools: &[AnthropicTool]) -> String {
    let defs = tools
        .iter()
        .map(format_tool)
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "{}\n\n### 可用工具\n\n{}\n{}",
        TOOL_CALL_FORMAT, defs, TOOL_CALL_REMINDER
    )
}

fn format_tool(tool: &AnthropicTool) -> String {
    let params = format_tool_params(tool);
    format!(
        "- **{}** (function)\n  - 调用示例: `<|tool▁calls▁begin|>[{{\"name\": \"{}\", \"arguments\": {}}}]<|tool▁calls▁end|>`\n  - 说明:\n~~~markdown\n{}\n~~~\n  - 参数:\n{}",
        tool.name, tool.name, params, tool.description, params
    )
}

fn format_tool_params(tool: &AnthropicTool) -> String {
    let props = tool
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object());
    let required: Vec<&str> = tool
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    match props {
        Some(p) if !p.is_empty() => {
            let fields: Vec<String> = p
                .iter()
                .map(|(k, v)| {
                    let required_mark = if required.contains(&k.as_str()) { " (必填)" } else { "" };
                    let desc = v
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    let example = example_value_for_key(k, v);
                    format!("      - **{}**{}: {}  [示例: {}]", k, required_mark, desc, example)
                })
                .collect();
            let obj = format!("{{\n{}\n    }}", fields.join(",\n"));
            obj
        }
        _ => "{}".to_string(),
    }
}

fn example_value_for_key(key: &str, v: &serde_json::Value) -> String {
    let vtype = v.get("type").and_then(|t| t.as_str()).unwrap_or("string");
    match vtype {
        "string" => {
            if key.contains("path") || key.contains("file") {
                "\"/path/to/file\"".to_string()
            } else if key.contains("command") {
                "\"echo hello\"".to_string()
            } else if key.contains("pattern") {
                "\"*.ts\"".to_string()
            } else if key.contains("url") {
                "\"https://example.com\"".to_string()
            } else {
                "\"value\"".to_string()
            }
        }
        "number" | "integer" => "42".to_string(),
        "boolean" => "true".to_string(),
        "array" => "[]".to_string(),
        "object" => "{}".to_string(),
        _ => "\"value\"".to_string(),
    }
}

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

    if let Some(tools) = &req.tools {
        if !tools.is_empty() {
            let tool_block = build_tool_instruction_block(tools);
            system_parts.push(tool_block);
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
                            role_texts.push(format!(
                                "{}: <|tool\u{2581}calls\u{2581}begin|>\n{}\n<|tool\u{2581}calls\u{2581}end|>",
                                role_prefix,
                                serde_json::to_string(&serde_json::json!([{"name": name, "arguments": input}])).unwrap_or_default()
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

    let has_tools = req.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false);
    let force_tool = req.tool_choice.as_ref().map(|tc| tc.is_forced()).unwrap_or(false);

    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let mut text = text_parts.join("\n");

    if has_tools {
        if force_tool {
            text.push_str("\n\n[System]: 你必须使用工具来完成当前任务。只能回复工具调用，不能回复文字。");
        }
        // 借鉴 ds-free-api: 末尾注入不闭合的 <think> 块，让工具格式紧邻模型生成位置
        text.push_str("\n\n<think>\n现在开始分析任务。如果需要使用工具，我必须按照以下格式输出：\n\n<|tool\u{2581}calls\u{2581}begin|>\n[{\"name\": \"工具名\", \"arguments\": {\"参数名\": \"值\"}}]\n<|tool\u{2581}calls\u{2581}end|>\n\n");
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
            tool_choice: None,
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
            tool_choice: None,
        };
        let result = convert_anthropic_request(&req, "test", false).unwrap();
        assert!(result.text.contains("tool\u{2581}calls\u{2581}begin"));
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
            tool_choice: None,
        };
        assert!(convert_anthropic_request(&req, "test", false).is_err());
    }

    #[test]
    fn test_tool_instruction_block() {
        let tools = vec![AnthropicTool {
            name: "bash".to_string(),
            description: "Execute a command".to_string(),
            input_schema: serde_json::json!({
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
            }),
        }];
        let block = build_tool_instruction_block(&tools);
        assert!(block.contains("工具调用"));
        assert!(block.contains("tool\u{2581}calls\u{2581}begin"));
        assert!(block.contains("bash"));
        assert!(block.contains("Execute a command"));
        assert!(block.contains("必填"));
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
        let tool_pos = sys.find("工具调用").unwrap();
        let system_pos = sys.find("You are helpful").unwrap();
        assert!(tool_pos < system_pos, "工具指令应在系统提示之前");
    }
}
