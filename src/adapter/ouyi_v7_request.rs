//! OpenAI / Anthropic 请求到 ouyi 7.0 结构化输入的适配。
//!
//! 将两种协议的多轮消息归一为 [`Ouyi7ChatInput`]：system 消息合并为系统提示、
//! 最后一条用户消息独立为当前输入 content、其余消息截断后作为历史列表，
//! 工具调用与工具结果以标记文本形式保留在历史中。
use crate::error::AppError;
use crate::model::anthropic::{
    AnthropicContent, ContentBlock, MessagesRequest, SystemBlock, SystemContent,
};
use crate::model::openai::{ChatCompletionRequest, ChatMessage};
use crate::model::ouyi_v7::Ouyi7HistoryMessage;

/// ouyi 7.0 聊天输入结构，由请求适配函数构造后交给上游客户端发送。
#[derive(Debug, Clone)]
pub struct Ouyi7ChatInput {
    /// 目标模型名。
    pub model: String,
    /// 合并后的系统提示文本；请求中无 system 内容时为空字符串。
    pub system_message: String,
    /// 历史消息列表（已按上限截断，system 提示位于首位；不含当前输入）。
    pub messages: Vec<Ouyi7HistoryMessage>,
    /// 当前这轮的用户输入文本；请求中没有用户消息时为空字符串。
    pub content: String,
    /// 透传的采样温度；请求未指定时为 `None`。
    pub temperature: Option<f64>,
    /// 透传的最大生成长度；请求未指定时为 `None`。
    pub max_tokens: Option<u64>,
}

/// 将 OpenAI ChatCompletion 请求转换为 ouyi 7.0 输入。
///
/// - `req`：OpenAI 格式请求，含多轮消息与采样参数；
/// - `model`：目标模型名；
/// - `max_history_messages`：历史消息保留上限（system 提示不计入）。
///
/// 转换规则：system 消息合并进 `system_message`；assistant 消息携带的
/// tool_calls 以工具调用标记文本保留在历史中；tool 结果按 `tool_call_id`
/// 反查工具名后以 `<tool_result>` 包裹并入历史；最后一条用户消息从历史中
/// 取出作为当前输入 `content`，剩余历史经 [`truncate_history`] 截断。
///
/// 返回 [`Ouyi7ChatInput`]；`messages` 为空或 tool 结果找不到对应工具名时
/// 返回 `Err`（`AppError::BadRequest`）。
pub fn from_openai_chat(
    req: &ChatCompletionRequest,
    model: String,
    max_history_messages: usize,
) -> Result<Ouyi7ChatInput, AppError> {
    if req.messages.is_empty() {
        return Err(AppError::BadRequest("messages 不能为空".to_string()));
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut conversation: Vec<Ouyi7HistoryMessage> = Vec::new();
    let tool_names = crate::tool_runtime::context::openai_tool_name_index(req);

    for msg in &req.messages {
        let msg_content = get_openai_message_content(msg);

        match msg.role.as_str() {
            "system" => {
                if !msg_content.is_empty() {
                    system_parts.push(msg_content);
                }
            }
            "user" => {
                conversation.push(Ouyi7HistoryMessage {
                    role: "user".to_string(),
                    content: msg_content,
                    content_files: vec![],
                });
            }
            "assistant" => {
                if let Some(tool_calls) = &msg.tool_calls {
                    let calls_text = format_tool_calls_as_text(tool_calls);
                    conversation.push(Ouyi7HistoryMessage {
                        role: "assistant".to_string(),
                        content: calls_text,
                        content_files: vec![],
                    });
                } else if !msg_content.is_empty() {
                    conversation.push(Ouyi7HistoryMessage {
                        role: "assistant".to_string(),
                        content: msg_content,
                        content_files: vec![],
                    });
                }
            }
            "tool" => {
                let tool_name = msg
                    .name
                    .as_deref()
                    .or_else(|| {
                        msg.tool_call_id
                            .as_ref()
                            .and_then(|id| tool_names.get(id))
                            .map(String::as_str)
                    })
                    .ok_or_else(|| {
                        AppError::BadRequest(format!(
                            "工具结果 {} 找不到对应工具名",
                            msg.tool_call_id.as_deref().unwrap_or("<missing>")
                        ))
                    })?;
                let tool_result_text = format!(
                    "<tool_result name=\"{}\">{}</tool_result>",
                    tool_name, msg_content
                );
                conversation.push(Ouyi7HistoryMessage {
                    role: "user".to_string(),
                    content: tool_result_text,
                    content_files: vec![],
                });
            }
            _ => {
                conversation.push(Ouyi7HistoryMessage {
                    role: msg.role.clone(),
                    content: msg_content,
                    content_files: vec![],
                });
            }
        }
    }

    // 找到最后一个 role=="user" 的索引
    let last_user_idx = conversation.iter().rposition(|m| m.role == "user");

    let (history_messages, content) = if let Some(idx) = last_user_idx {
        // 移除最后一个 user 消息作为当前 content
        let current = conversation.remove(idx);
        (conversation, current.content)
    } else {
        (conversation, String::new())
    };

    let system_message = system_parts.join("\n\n");

    let truncated_history = truncate_history(
        if system_message.is_empty() {
            vec![]
        } else {
            vec![Ouyi7HistoryMessage {
                role: "system".to_string(),
                content: system_message.clone(),
                content_files: vec![],
            }]
        },
        history_messages,
        max_history_messages,
    );

    Ok(Ouyi7ChatInput {
        model,
        system_message,
        messages: truncated_history,
        content,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
    })
}

/// 将 Anthropic Messages 请求转换为 ouyi 7.0 输入。
///
/// - `req`：Anthropic 格式请求（`max_tokens` 为必填字段）；
/// - `model`：目标模型名；
/// - `max_history_messages`：历史消息保留上限（system 提示不计入）。
///
/// 转换规则：请求自带 system 合并进 `system_message`；user 消息中的
/// tool_result 块转为 `<tool_result>` 文本、assistant 消息中的 tool_use 块
/// 转为 `<tool_use>` 文本保留在历史中；最后一条用户消息从历史中取出作为
/// 当前输入 `content`，剩余历史经 [`truncate_history`] 截断。
///
/// 返回 [`Ouyi7ChatInput`]；`messages` 为空时返回 `Err`（`AppError::BadRequest`）。
pub fn from_anthropic_messages(
    req: &MessagesRequest,
    model: String,
    max_history_messages: usize,
) -> Result<Ouyi7ChatInput, AppError> {
    if req.messages.is_empty() {
        return Err(AppError::BadRequest("messages 不能为空".to_string()));
    }

    let mut system_parts: Vec<String> = Vec::new();

    if let Some(system) = &req.system {
        let sys_text = extract_system_text(system);
        if !sys_text.is_empty() {
            system_parts.push(sys_text);
        }
    }

    let mut conversation: Vec<Ouyi7HistoryMessage> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "user" => {
                let text = extract_anthropic_content_text(&msg.content);
                conversation.push(Ouyi7HistoryMessage {
                    role: "user".to_string(),
                    content: text,
                    content_files: vec![],
                });
            }
            "assistant" => {
                let text = extract_anthropic_assistant_text(&msg.content);
                if !text.is_empty() {
                    conversation.push(Ouyi7HistoryMessage {
                        role: "assistant".to_string(),
                        content: text,
                        content_files: vec![],
                    });
                }
            }
            _ => {
                let text = extract_anthropic_content_text(&msg.content);
                conversation.push(Ouyi7HistoryMessage {
                    role: msg.role.clone(),
                    content: text,
                    content_files: vec![],
                });
            }
        }
    }

    // 找到最后一个 role=="user" 的索引
    let last_user_idx = conversation.iter().rposition(|m| m.role == "user");

    let (history_messages, content) = if let Some(idx) = last_user_idx {
        let current = conversation.remove(idx);
        (conversation, current.content)
    } else {
        (conversation, String::new())
    };

    let system_message = system_parts.join("\n\n");

    let truncated_history = truncate_history(
        if system_message.is_empty() {
            vec![]
        } else {
            vec![Ouyi7HistoryMessage {
                role: "system".to_string(),
                content: system_message.clone(),
                content_files: vec![],
            }]
        },
        history_messages,
        max_history_messages,
    );

    Ok(Ouyi7ChatInput {
        model,
        system_message,
        messages: truncated_history,
        content,
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
    })
}

/// 按上限截断消息列表，优先保留 system 与最近的消息。
///
/// - `system_messages`：置于结果头部的 system 消息；
/// - `history_messages`：待截断的历史消息；
/// - `max_history_messages`：结果总长度上限。
///
/// 历史未超限时原样拼接返回；超限时按上限扣除 system 占用名额后
/// 从末尾保留尽可能多的最近消息。返回拼接后的消息列表。
pub fn truncate_history(
    system_messages: Vec<Ouyi7HistoryMessage>,
    history_messages: Vec<Ouyi7HistoryMessage>,
    max_history_messages: usize,
) -> Vec<Ouyi7HistoryMessage> {
    let system_count = system_messages.len();

    if history_messages.len() <= max_history_messages {
        let mut result = system_messages;
        result.extend(history_messages);
        return result;
    }

    let available_for_history = max_history_messages.saturating_sub(system_count);
    let take_from_end = available_for_history.min(history_messages.len());

    let mut result = system_messages;
    let start_index = history_messages.len().saturating_sub(take_from_end);
    result.extend(history_messages.into_iter().skip(start_index));

    result
}

fn get_openai_message_content(msg: &ChatMessage) -> String {
    match &msg.content {
        Some(c) => c.clone(),
        None => String::new(),
    }
}

fn format_tool_calls_as_text(tool_calls: &[crate::model::openai::ToolCall]) -> String {
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

    format!(
        "<|tool\u{2581}calls\u{2581}begin|>\n{}\n<|tool\u{2581}calls\u{2581}end|>",
        serde_json::to_string(&calls).unwrap_or_default()
    )
}

fn extract_system_text(system: &SystemContent) -> String {
    match system {
        SystemContent::Text(text) => text.clone(),
        SystemContent::Blocks(blocks) => {
            let texts: Vec<String> = blocks
                .iter()
                .filter_map(|b: &SystemBlock| {
                    if b.r#type == "text" {
                        Some(b.text.clone())
                    } else {
                        None
                    }
                })
                .collect();
            texts.join("\n")
        }
    }
}

fn extract_anthropic_content_text(content: &AnthropicContent) -> String {
    match content {
        AnthropicContent::Text(text) => text.clone(),
        AnthropicContent::Blocks(blocks) => {
            let texts: Vec<String> = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Some(format!(
                        "<tool_result name=\"{}\">{}</tool_result>",
                        tool_use_id, content
                    )),
                    _ => None,
                })
                .collect();
            texts.join("\n")
        }
    }
}

fn extract_anthropic_assistant_text(content: &AnthropicContent) -> String {
    match content {
        AnthropicContent::Text(text) => text.clone(),
        AnthropicContent::Blocks(blocks) => {
            let texts: Vec<String> = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    ContentBlock::ToolUse { id, name, input } => Some(format!(
                        "<tool_use id=\"{}\" name=\"{}\">\n{}\n</tool_use>",
                        id,
                        name,
                        serde_json::to_string(input).unwrap_or_default()
                    )),
                    _ => None,
                })
                .collect();
            texts.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::anthropic::{AnthropicMessage, SystemContent};
    use crate::model::openai::{ChatCompletionRequest, ChatMessage};

    #[test]
    fn test_openai_tool_result_resolves_name_from_prior_call_id() {
        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model":"test",
            "messages":[
                {"role":"assistant","tool_calls":[{
                    "id":"call_1","type":"function",
                    "function":{"name":"Read","arguments":"{\"file_path\":\"a.rs\"}"}
                }]},
                {"role":"tool","tool_call_id":"call_1","content":"contents"}
            ]
        }))
        .unwrap();

        let converted = from_openai_chat(&req, "test".to_string(), 20).unwrap();

        assert!(converted.content.contains("<tool_result name=\"Read\">"));
        assert!(!converted.content.contains("name=\"unknown\""));
    }

    fn make_openai_request(messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test".to_string(),
            messages,
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        }
    }

    fn make_openai_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn test_openai_basic_user_message() {
        let req = make_openai_request(vec![make_openai_message("user", "hello")]);
        let result = from_openai_chat(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(result.system_message, "");
        assert!(result.messages.is_empty());
    }

    #[test]
    fn test_openai_system_and_user() {
        let req = make_openai_request(vec![
            make_openai_message("system", "You are helpful"),
            make_openai_message("user", "hi"),
        ]);
        let result = from_openai_chat(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.system_message, "You are helpful");
        assert_eq!(result.content, "hi");
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, "system");
    }

    #[test]
    fn test_openai_multiple_system_merged() {
        let req = make_openai_request(vec![
            make_openai_message("system", "Rule 1"),
            make_openai_message("system", "Rule 2"),
            make_openai_message("user", "hello"),
        ]);
        let result = from_openai_chat(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.system_message, "Rule 1\n\nRule 2");
    }

    #[test]
    fn test_openai_last_user_as_content() {
        let req = make_openai_request(vec![
            make_openai_message("user", "first question"),
            make_openai_message("assistant", "answer"),
            make_openai_message("user", "second question"),
        ]);
        let result = from_openai_chat(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.content, "second question");
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content, "first question");
        assert_eq!(result.messages[1].role, "assistant");
    }

    #[test]
    fn test_openai_tool_result_in_history() {
        let req = make_openai_request(vec![
            make_openai_message("user", "read file"),
            ChatMessage {
                role: "tool".to_string(),
                content: Some("file content".to_string()),
                tool_calls: None,
                tool_call_id: Some("call_123".to_string()),
                name: Some("read_file".to_string()),
            },
            make_openai_message("user", "next question"),
        ]);
        let result = from_openai_chat(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.content, "next question");
        assert_eq!(result.messages.len(), 2);
        assert!(result.messages[1].content.contains("<tool_result"));
        assert!(result.messages[1].content.contains("read_file"));
    }

    #[test]
    fn test_openai_history_truncation() {
        let messages: Vec<ChatMessage> = (0..30)
            .map(|i| {
                make_openai_message(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("msg {}", i),
                )
            })
            .collect();
        let req = make_openai_request(messages);
        let result = from_openai_chat(&req, "ouyi-chat".to_string(), 10).unwrap();
        assert!(result.messages.len() <= 10);
    }

    #[test]
    fn test_openai_empty_messages_rejected() {
        let req = make_openai_request(vec![]);
        assert!(from_openai_chat(&req, "ouyi-chat".to_string(), 20).is_err());
    }

    #[test]
    fn test_truncate_history_preserves_system() {
        let system = vec![Ouyi7HistoryMessage {
            role: "system".to_string(),
            content: "system prompt".to_string(),
            content_files: vec![],
        }];
        let history: Vec<Ouyi7HistoryMessage> = (0..25)
            .map(|i| Ouyi7HistoryMessage {
                role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: format!("msg {}", i),
                content_files: vec![],
            })
            .collect();

        let result = truncate_history(system, history, 10);
        assert_eq!(result.len(), 10);
        assert_eq!(result[0].role, "system");
        assert!(
            result.last().unwrap().content.contains("msg 2")
                || result.last().unwrap().content.contains("msg 24")
        );
    }

    #[test]
    fn test_truncate_history_no_system() {
        let history: Vec<Ouyi7HistoryMessage> = (0..25)
            .map(|i| Ouyi7HistoryMessage {
                role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: format!("msg {}", i),
                content_files: vec![],
            })
            .collect();

        let result = truncate_history(vec![], history, 10);
        assert_eq!(result.len(), 10);
        assert!(result.last().unwrap().content.contains("msg 24"));
    }

    fn make_anthropic_request(
        messages: Vec<AnthropicMessage>,
        system: Option<String>,
    ) -> MessagesRequest {
        MessagesRequest {
            model: "claude-3".to_string(),
            messages,
            system: system.map(SystemContent::Text),
            max_tokens: 1024,
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

    fn make_anthropic_message(role: &str, content: &str) -> AnthropicMessage {
        AnthropicMessage {
            role: role.to_string(),
            content: AnthropicContent::Text(content.to_string()),
        }
    }

    #[test]
    fn test_anthropic_basic_user_message() {
        let req = make_anthropic_request(vec![make_anthropic_message("user", "hello")], None);
        let result = from_anthropic_messages(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(result.system_message, "");
    }

    #[test]
    fn test_anthropic_system_field() {
        let req = make_anthropic_request(
            vec![make_anthropic_message("user", "hi")],
            Some("You are helpful".to_string()),
        );
        let result = from_anthropic_messages(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.system_message, "You are helpful");
    }

    #[test]
    fn test_anthropic_last_user_as_content() {
        let req = make_anthropic_request(
            vec![
                make_anthropic_message("user", "first"),
                make_anthropic_message("assistant", "answer"),
                make_anthropic_message("user", "second"),
            ],
            None,
        );
        let result = from_anthropic_messages(&req, "ouyi-chat".to_string(), 20).unwrap();
        assert_eq!(result.content, "second");
        assert_eq!(result.messages.len(), 2);
    }

    #[test]
    fn test_anthropic_empty_messages_rejected() {
        let req = make_anthropic_request(vec![], None);
        assert!(from_anthropic_messages(&req, "ouyi-chat".to_string(), 20).is_err());
    }
}
