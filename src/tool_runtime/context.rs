use std::collections::HashMap;

use crate::model::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use crate::model::openai::ChatCompletionRequest;

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    UserText {
        content: String,
    },
    AssistantText {
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
}

pub fn openai_tool_name_index(req: &ChatCompletionRequest) -> HashMap<String, String> {
    req.messages
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flatten()
        .map(|call| (call.id.clone(), call.function.name.clone()))
        .collect()
}

pub fn from_openai_chat(req: &ChatCompletionRequest) -> Result<Vec<TranscriptItem>, String> {
    let mut transcript = Vec::new();
    let tool_names = openai_tool_name_index(req);

    for message in &req.messages {
        match message.role.as_str() {
            "user" => push_text(&mut transcript, true, message.content.as_deref()),
            "assistant" => {
                push_text(&mut transcript, false, message.content.as_deref());
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        let input =
                            serde_json::from_str(&call.function.arguments).map_err(|error| {
                                format!("工具调用 {} 参数 JSON 无效: {error}", call.id)
                            })?;
                        transcript.push(TranscriptItem::ToolCall {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            input,
                        });
                    }
                }
            }
            "tool" => {
                let id = message
                    .tool_call_id
                    .as_ref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "OpenAI 工具结果缺少 tool_call_id".to_string())?;
                let name = message
                    .name
                    .clone()
                    .or_else(|| tool_names.get(id).cloned())
                    .ok_or_else(|| format!("OpenAI 工具结果 {id} 找不到对应工具调用"))?;
                transcript.push(TranscriptItem::ToolResult {
                    id: id.clone(),
                    name,
                    output: message.content.clone().unwrap_or_default(),
                    is_error: false,
                });
            }
            _ => {}
        }
    }

    Ok(transcript)
}

pub fn from_anthropic_messages(req: &MessagesRequest) -> Result<Vec<TranscriptItem>, String> {
    let mut transcript = Vec::new();
    let mut tool_names = HashMap::<String, String>::new();

    for message in &req.messages {
        match &message.content {
            AnthropicContent::Text(text) => {
                push_text(&mut transcript, message.role == "user", Some(text));
            }
            AnthropicContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            push_text(&mut transcript, message.role == "user", Some(text));
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_names.insert(id.clone(), name.clone());
                            transcript.push(TranscriptItem::ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            });
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let name = tool_names.get(tool_use_id).cloned().ok_or_else(|| {
                                format!("Anthropic 工具结果 {tool_use_id} 找不到对应 tool_use")
                            })?;
                            transcript.push(TranscriptItem::ToolResult {
                                id: tool_use_id.clone(),
                                name,
                                output: content.clone(),
                                is_error: *is_error,
                            });
                        }
                        ContentBlock::Thinking { .. } => {}
                    }
                }
            }
        }
    }

    Ok(transcript)
}

fn push_text(transcript: &mut Vec<TranscriptItem>, user: bool, content: Option<&str>) {
    let Some(content) = content.filter(|content| !content.is_empty()) else {
        return;
    };
    if user {
        transcript.push(TranscriptItem::UserText {
            content: content.to_string(),
        });
    } else {
        transcript.push(TranscriptItem::AssistantText {
            content: content.to_string(),
        });
    }
}

pub fn render_for_compiler(transcript: &[TranscriptItem]) -> String {
    transcript
        .iter()
        .map(|item| match item {
            TranscriptItem::UserText { content } => format!("[user]\n{content}"),
            TranscriptItem::AssistantText { content } => format!("[assistant]\n{content}"),
            TranscriptItem::ToolCall { id, name, input } => {
                format!("[tool_call id={id} name={name}]\n{input}")
            }
            TranscriptItem::ToolResult {
                id,
                name,
                output,
                is_error,
            } => {
                let output: String = output.chars().take(4_000).collect();
                format!("[tool_result id={id} name={name} error={is_error}]\n{output}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_transcript_resolves_tool_name_by_call_id() {
        let req: crate::model::openai::ChatCompletionRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-test",
                "messages": [
                    {"role":"user","content":"检查项目"},
                    {"role":"assistant","tool_calls":[{
                        "id":"call_read","type":"function",
                        "function":{"name":"Read","arguments":"{\"file_path\":\"missing.rs\"}"}
                    }]},
                    {"role":"tool","tool_call_id":"call_read","content":"file not found"},
                    {"role":"assistant","tool_calls":[{
                        "id":"call_bash","type":"function",
                        "function":{"name":"Bash","arguments":"{\"command\":\"rg --files\"}"}
                    }]},
                    {"role":"tool","tool_call_id":"call_bash","content":"src/main.rs"},
                    {"role":"user","content":"继续"}
                ]
            }))
            .unwrap();

        let transcript = from_openai_chat(&req).unwrap();

        assert_eq!(transcript.len(), 6);
        assert!(matches!(
            &transcript[2],
            TranscriptItem::ToolResult { id, name, output, .. }
                if id == "call_read" && name == "Read" && output == "file not found"
        ));
        assert!(matches!(
            &transcript[4],
            TranscriptItem::ToolResult { id, name, .. }
                if id == "call_bash" && name == "Bash"
        ));
    }

    #[test]
    fn openai_transcript_rejects_unknown_tool_result_id() {
        let req: crate::model::openai::ChatCompletionRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-test",
                "messages": [{"role":"tool","tool_call_id":"missing","content":"x"}]
            }))
            .unwrap();

        assert!(from_openai_chat(&req).unwrap_err().contains("missing"));
    }

    #[test]
    fn anthropic_transcript_preserves_name_and_error_flag() {
        let req: crate::model::anthropic::MessagesRequest =
            serde_json::from_value(serde_json::json!({
                "model":"claude-test",
                "max_tokens":1024,
                "messages":[
                    {"role":"user","content":"读取文件"},
                    {"role":"assistant","content":[{
                        "type":"tool_use","id":"toolu_1","name":"Read",
                        "input":{"file_path":"missing.rs"}
                    }]},
                    {"role":"user","content":[{
                        "type":"tool_result","tool_use_id":"toolu_1",
                        "content":"not found","is_error":true
                    }]}
                ]
            }))
            .unwrap();

        let transcript = from_anthropic_messages(&req).unwrap();
        assert!(matches!(
            &transcript[2],
            TranscriptItem::ToolResult { id, name, is_error, .. }
                if id == "toolu_1" && name == "Read" && *is_error
        ));
    }
}
