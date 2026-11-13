//! 编译后的工具调用到 OpenAI 响应结构的构造器。
//!
//! 为工具模型路径提供统一的响应体与 SSE chunk 构造能力：
//! 非流式整体响应、以及流式序列所需的 role / tool_call 增量 / finish /
//! 文本 content / finish 各类 chunk。
use crate::model::openai::{
    ChatCompletionChunk, ChatCompletionResponse, ChunkChoice, ChunkDelta, ResponseChoice,
    ResponseMessage, ToolCall, ToolCallDelta, ToolCallFunction, ToolCallFunctionDelta, Usage,
};
use crate::tool_runtime::openai_compiler::CompiledToolCall;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 构造非流式工具调用 OpenAI 响应。
///
/// - `model` / `task_id`：响应中的模型名与响应 ID；
/// - `calls`：已编译的工具调用列表，`input` 序列化为 arguments JSON，
///   序列化失败时以 `{}` 兜底；
/// - `prompt_tokens` / `completion_tokens`：写入 usage，total 为两者之和。
///
/// 返回 `finish_reason` 为 `tool_calls`、`content` 为 `None` 的响应体。
pub fn build_openai_tool_call_response(
    model: &str,
    task_id: &str,
    calls: Vec<CompiledToolCall>,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> ChatCompletionResponse {
    let tool_calls = calls
        .into_iter()
        .map(|call| ToolCall {
            id: call.id,
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: call.name,
                arguments: serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".to_string()),
            },
        })
        .collect();

    ChatCompletionResponse {
        id: task_id.to_string(),
        object: "chat.completion".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ResponseChoice {
            index: 0,
            message: ResponseMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(tool_calls),
            },
            finish_reason: "tool_calls".to_string(),
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    }
}

/// 构造非流式纯文本 OpenAI 响应。
///
/// - `model` / `task_id`：响应中的模型名与响应 ID；
/// - `content`：助手回复的完整文本；
/// - `prompt_tokens` / `completion_tokens`：写入 usage，total 为两者之和。
///
/// 返回 `finish_reason` 为 `stop`、无工具调用的响应体。
pub fn build_openai_text_response(
    model: &str,
    task_id: &str,
    content: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: task_id.to_string(),
        object: "chat.completion".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ResponseChoice {
            index: 0,
            message: ResponseMessage {
                role: "assistant".to_string(),
                content: Some(content.to_string()),
                tool_calls: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    }
}

/// 构造流式工具调用序列的起始 chunk。
///
/// 仅携带 `role: assistant`，无内容增量，`finish_reason` 为 `None`。
pub fn build_tool_call_role_chunk(model: &str, task_id: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                role: Some("assistant".to_string()),
                content: None,
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
    }
}

/// 构造单个工具调用的增量 chunk。
///
/// - `idx`：该调用在多工具序列中的序号；
/// - `call`：已编译的工具调用，函数名与 arguments 直接透传。
pub fn build_tool_call_delta_chunk(
    model: &str,
    task_id: &str,
    idx: u32,
    call: &CompiledToolCall,
) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                role: None,
                content: None,
                tool_calls: Some(vec![ToolCallDelta {
                    index: idx,
                    id: Some(call.id.clone()),
                    call_type: Some("function".to_string()),
                    function: Some(ToolCallFunctionDelta {
                        name: Some(call.name.clone()),
                        arguments: Some(
                            serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".to_string()),
                        ),
                    }),
                }]),
            },
            finish_reason: None,
        }],
        usage: None,
    }
}

/// 构造工具调用序列的收尾 chunk，`finish_reason` 为 `tool_calls`，无内容增量。
pub fn build_tool_call_finish_chunk(model: &str, task_id: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                role: None,
                content: None,
                tool_calls: None,
            },
            finish_reason: Some("tool_calls".to_string()),
        }],
        usage: None,
    }
}

/// 构造流式文本序列的起始 chunk，仅携带 `role: assistant`，无内容增量。
pub fn build_text_role_chunk(model: &str, task_id: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                role: Some("assistant".to_string()),
                content: None,
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
    }
}

/// 构造文本内容增量 chunk。
///
/// - `content`：本次到达的文本片段，原样放入 delta。
pub fn build_text_content_chunk(model: &str, task_id: &str, content: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                role: None,
                content: Some(content.to_string()),
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
    }
}

/// 构造文本流的收尾 chunk，`finish_reason` 为 `stop`，无内容增量。
pub fn build_text_finish_chunk(model: &str, task_id: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: now_unix_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: ChunkDelta {
                role: None,
                content: None,
                tool_calls: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tool_call_response() {
        let calls = vec![CompiledToolCall {
            id: "call_1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "pwd"}),
        }];

        let resp = build_openai_tool_call_response("gpt-test", "chatcmpl-test", calls, 0, 0);

        assert_eq!(resp.choices[0].finish_reason, "tool_calls");
        assert_eq!(
            resp.choices[0].message.tool_calls.as_ref().unwrap()[0]
                .function
                .name,
            "Bash"
        );
        assert_eq!(
            resp.choices[0].message.tool_calls.as_ref().unwrap()[0]
                .function
                .arguments,
            "{\"command\":\"pwd\"}"
        );
    }

    #[test]
    fn test_build_text_response() {
        let resp = build_openai_text_response("gpt-test", "chatcmpl-test", "Hello world", 10, 5);

        assert_eq!(resp.choices[0].finish_reason, "stop");
        assert_eq!(
            resp.choices[0].message.content.as_ref().unwrap(),
            "Hello world"
        );
        assert_eq!(resp.usage.total_tokens, 15);
    }

    #[test]
    fn test_build_tool_call_response_with_multiple_calls() {
        let calls = vec![
            CompiledToolCall {
                id: "call_1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "Cargo.toml"}),
            },
            CompiledToolCall {
                id: "call_2".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cargo test"}),
            },
        ];

        let resp = build_openai_tool_call_response("gpt-test", "chatcmpl-test", calls, 100, 50);

        assert_eq!(
            resp.choices[0].message.tool_calls.as_ref().unwrap().len(),
            2
        );
        assert_eq!(resp.usage.prompt_tokens, 100);
        assert_eq!(resp.usage.completion_tokens, 50);
    }

    #[test]
    fn test_build_tool_call_chunks() {
        let call = CompiledToolCall {
            id: "call_1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "Cargo.toml"}),
        };

        let role = build_tool_call_role_chunk("gpt-test", "chatcmpl-1");
        assert!(role.choices[0].delta.role.is_some());
        assert!(role.choices[0].delta.tool_calls.is_none());

        let delta = build_tool_call_delta_chunk("gpt-test", "chatcmpl-1", 0, &call);
        assert!(delta.choices[0].delta.tool_calls.is_some());
        let tc = &delta.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function.as_ref().unwrap().name.as_ref().unwrap(), "Read");

        let finish = build_tool_call_finish_chunk("gpt-test", "chatcmpl-1");
        assert_eq!(
            finish.choices[0].finish_reason.as_ref().unwrap(),
            "tool_calls"
        );
    }
}
