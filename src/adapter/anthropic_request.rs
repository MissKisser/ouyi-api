//! Anthropic Messages 请求到 ouyi 文本协议的请求适配。
//!
//! 将消息列表（含 thinking / tool_use / tool_result 块）拍平为带角色前缀的
//! 单段文本，注入工具协作协议说明（长正文经 ouyi_payload 块承载、短意图用
//! 自然语言表达），并对超长工具结果做 UTF-8 安全截断。
use crate::model::anthropic::{
    AnthropicContent, AnthropicTool, ContentBlock, MessagesRequest, SystemContent,
};

/// 转换后的 ouyi 请求载荷。
pub struct OuyiRequest {
    /// 拼接后的系统提示（工具协作协议在前、请求自带 system 在后）；无内容时为 `None`。
    pub system_prompt: Option<String>,
    /// 拍平后的对话正文，角色以 `[User]:` / `[Assistant]:` 等前缀标注，
    /// 工具调用以标记块内嵌、工具结果以 `<tool_result>` 包裹。
    pub text: String,
    /// 是否启用思考模式；调用方判定为需要思考或模型名包含 `-thinking` 时为真。
    pub thinking: bool,
}

const TOOL_CALL_FORMAT: &str = concat!(
    "## 工具协作协议\n\n",
    "你是主脑模型，负责判断任务、生成报告、代码、方案和文件正文。系统会在服务端将你的工具意图转换为 Claude Code 工具调用。\n\n",
    "### 长文本 payload\n",
    "当你需要通过 Write、Edit、MultiEdit 等工具提交长文本字段时，正文写在 payload 块中：\n",
    "```text\n",
    "<ouyi_payload id=\"p1\">\n",
    "这里写入完整正文，包含换行、引号、代码块均可。\n",
    "</ouyi_payload>\n",
    "```\n\n",
    "随后用自然语言说明工具意图，例如：将 payload p1 写入 docs/report.md。\n\n",
    "### 短工具意图\n",
    "需要执行命令时，可以写：执行命令：你的命令\n",
    "需要读取文件时，可以写：读取文件 路径\n\n",
    "### 工具协议层\n",
    "工具模型只会编译工具调用协议，不会生成或改写 payload 正文。长文本字段会以 content_ref、old_string_ref、new_string_ref 引用 payload id。\n"
);

const TOOL_CALL_REMINDER: &str = "\n\n【工具调用提醒】如果需要使用工具，请表达工具意图；长文本放入 ouyi_payload，服务端会转换为 Claude Code 工具调用。";
const TOOL_RESULT_CONTENT_MAX_BYTES: usize = 64 * 1024;

fn truncate_utf8_by_byte_limit_with_notice(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let char_count = content.chars().count();
    let truncated: String = content
        .chars()
        .scan(0usize, |bytes, ch| {
            let next = *bytes + ch.len_utf8();
            if next > max_bytes {
                None
            } else {
                *bytes = next;
                Some(ch)
            }
        })
        .collect();
    format!("{truncated}\n\n[...内容截断，共 {char_count} 字符...]")
}

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
    let example = if matches!(tool.name.as_str(), "Write" | "Edit" | "MultiEdit") {
        "长文本字段使用 <ouyi_payload id=\"p1\">...</ouyi_payload>，工具协议层使用 content_ref 或 *_ref 引用。".to_string()
    } else {
        format!("参数示例: {}", params)
    };
    format!(
        "- **{}** (function)\n  - 说明:\n~~~markdown\n{}\n~~~\n  - {}\n",
        tool.name, tool.description, example
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
                    let required_mark = if required.contains(&k.as_str()) {
                        " (必填)"
                    } else {
                        ""
                    };
                    let desc = v.get("description").and_then(|d| d.as_str()).unwrap_or("");
                    let example = example_value_for_key(k, v);
                    format!(
                        "      - **{}**{}: {}  [示例: {}]",
                        k, required_mark, desc, example
                    )
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

/// 将 Anthropic Messages 请求转换为 ouyi 文本协议请求。
///
/// - `req`：Anthropic 格式请求，消息 content 支持纯文本或块数组（text /
///   thinking / tool_use / tool_result）；
/// - `model_full`：完整模型名，包含 `-thinking` 时启用思考模式；
/// - `should_thinking`：调用方显式指定的思考开关，与模型名判定取或。
///
/// 转换规则：携带工具定义时把工具协作协议注入 `system_prompt`（位于请求
/// 自带 system 之前）；thinking 块转为 `<think>` 文本；tool_use 块转为工具
/// 调用标记块并登记 id → 工具名映射；tool_result 块按映射还原工具名后以
/// `<tool_result>` 包裹，正文超过 64 KiB 时按 UTF-8 边界截断并附加截断说明；
/// `tool_choice` 指定强制工具时在正文末尾注入强制提示与未闭合的 `<think>` 引导块。
///
/// 返回拼装好的 [`OuyiRequest`]；`messages` 为空时返回 `Err`。
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
                            ..
                        } => {
                            let tool_name = tool_id_to_name
                                .get(tool_use_id)
                                .map(|s| s.as_str())
                                .unwrap_or(tool_use_id);
                            let display_content = truncate_utf8_by_byte_limit_with_notice(
                                content,
                                TOOL_RESULT_CONTENT_MAX_BYTES,
                            );
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
    let force_tool = req
        .tool_choice
        .as_ref()
        .map(|tc| tc.is_forced())
        .unwrap_or(false);

    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let mut text = text_parts.join("\n");

    if has_tools {
        if let Some(crate::model::anthropic::ToolChoice::Tool { name }) = &req.tool_choice {
            text.push_str(&format!(
                "\n\n[System]: 你必须调用名为 {name} 的工具。只能回复该工具调用，不能回复文字。"
            ));
        } else if force_tool {
            text.push_str(
                "\n\n[System]: 你必须使用工具来完成当前任务。只能回复工具调用，不能回复文字。",
            );
        }
        text.push_str("\n\n<think>\n现在开始分析任务。需要工具时，请表达工具意图；长文本工具字段使用 ouyi_payload 块承载正文。\n\n");
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
    fn test_tool_result_preserves_typical_multibyte_file_output() {
        let req = make_req(AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "方".repeat(1000),
            is_error: false,
        }]));

        let result = convert_anthropic_request(&req, "test", false).unwrap();

        assert!(!result.text.contains("内容截断"));
        assert!(result.text.contains(&"方".repeat(1000)));
    }

    #[test]
    fn test_tool_result_truncates_extreme_output_with_notice() {
        let req = make_req(AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "x".repeat(TOOL_RESULT_CONTENT_MAX_BYTES + 1),
            is_error: false,
        }]));

        let result = convert_anthropic_request(&req, "test", false).unwrap();

        assert!(result.text.contains(&format!(
            "内容截断，共 {} 字符",
            TOOL_RESULT_CONTENT_MAX_BYTES + 1
        )));
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
        assert!(block.contains("工具协作协议"));
        assert!(block.contains("ouyi_payload"));
        assert!(block.contains("bash"));
        assert!(block.contains("Execute a command"));
        assert!(block.contains("必填"));
        assert!(block.contains("短工具意图"));
    }

    #[test]
    fn test_tool_instruction_uses_payload_protocol() {
        let tools = vec![AnthropicTool {
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
        let block = build_tool_instruction_block(&tools);
        assert!(block.contains("<ouyi_payload id=\"p1\">"));
        assert!(block.contains("content_ref"));
        assert!(!block.contains("直接输出工具调用标记"));
        assert!(!block.contains("<|tool\u{2581}calls\u{2581}begin|>[{\"name\": \"Write\""));
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

    #[test]
    fn test_specific_tool_choice_names_the_required_tool() {
        let mut req = make_req(AnthropicContent::Text("inspect".to_string()));
        req.tools = Some(vec![AnthropicTool {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
        }]);
        req.tool_choice = Some(crate::model::anthropic::ToolChoice::Tool {
            name: "Read".to_string(),
        });

        let converted = convert_anthropic_request(&req, "test", false).unwrap();

        assert!(converted.text.contains("必须调用名为 Read 的工具"));
    }
}
