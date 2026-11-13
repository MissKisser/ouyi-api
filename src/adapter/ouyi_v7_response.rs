//! 纯文本到各协议响应体 / 模拟流式 SSE 的构造器。
//!
//! 在只能拿到完整纯文本的降级场景下，按 OpenAI ChatCompletion、
//! Anthropic Messages、OpenAI Responses 三种协议分别构造非流式响应，
//! 或按 `chunk_chars` 字符切片生成一次性写出的模拟流式 SSE 响应；
//! 各构造器的 usage 均为零值，事件时序与真实流式协议保持一致。
use crate::error::AppError;
use crate::model::anthropic::{MessagesResponse, ResponseContentBlock, ResponseUsage};
use crate::model::openai::{
    ChatCompletionChunk, ChatCompletionResponse, ChunkChoice, ChunkDelta, ResponseChoice,
    ResponseContentPart, ResponseMessage, ResponseOutputItem, ResponseUsage as OpenAIResponseUsage,
    ResponsesResponse, Usage,
};
use axum::body::Body;
use axum::response::Response;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 构造非流式 OpenAI 文本响应。
///
/// - `model` / `task_id`：响应中的模型名与响应 ID；
/// - `text`：完整回复文本。
///
/// 返回 `finish_reason` 为 `stop`、usage 为零值的响应体。
pub fn build_openai_text_response_from_plain(
    model: &str,
    task_id: &str,
    text: &str,
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
                content: Some(text.to_string()),
                tool_calls: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
    }
}

/// 将纯文本切分为多个 chunk，构造模拟流式 OpenAI SSE 响应。
///
/// - `model` / `task_id`：写入每个 chunk 的模型名与响应 ID；
/// - `text`：完整回复文本，按字符（非字节）切片；
/// - `chunk_chars`：每个内容 chunk 的字符数。
///
/// 事件序列为 role chunk、若干 content chunk、finish chunk（`stop`）与
/// `[DONE]` 行。返回一次性携带完整 `text/event-stream` 响应体的 axum
/// `Response`（不做逐 chunk 真实推送）；响应体构建失败时返回 `Err`
/// （`AppError::Internal`）。
pub fn build_openai_text_stream_from_plain(
    model: &str,
    task_id: &str,
    text: &str,
    chunk_chars: usize,
) -> Result<Response, AppError> {
    let created = now_unix_secs();
    let mut chunks: Vec<String> = Vec::new();

    // Role chunk
    let role_chunk = ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
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
    };
    chunks.push(format!(
        "data: {}\n\n",
        serde_json::to_string(&role_chunk).unwrap_or_default()
    ));

    // Content chunks
    let chars: Vec<char> = text.chars().collect();
    for i in (0..chars.len()).step_by(chunk_chars) {
        let end = (i + chunk_chars).min(chars.len());
        let content: String = chars[i..end].iter().collect();

        let content_chunk = ChatCompletionChunk {
            id: task_id.to_string(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model.to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        chunks.push(format!(
            "data: {}\n\n",
            serde_json::to_string(&content_chunk).unwrap_or_default()
        ));
    }

    // Finish chunk
    let finish_chunk = ChatCompletionChunk {
        id: task_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
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
    };
    chunks.push(format!(
        "data: {}\n\n",
        serde_json::to_string(&finish_chunk).unwrap_or_default()
    ));

    // [DONE]
    chunks.push("data: [DONE]\n\n".to_string());

    let body = chunks.join("");
    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::from(body))
        .map_err(|e| AppError::Internal(e.to_string()))?)
}

/// 构造非流式 Anthropic 消息响应。
///
/// - `model` / `message_id`：响应中的模型名与消息 ID；
/// - `text`：完整回复文本。
///
/// 返回单个 text content block、`stop_reason` 为 `end_turn`、
/// usage 为零值的响应体。
pub fn build_anthropic_message_from_plain(
    model: &str,
    message_id: &str,
    text: &str,
) -> MessagesResponse {
    MessagesResponse {
        id: message_id.to_string(),
        r#type: "message",
        role: "assistant",
        model: model.to_string(),
        content: vec![ResponseContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: "end_turn".to_string(),
        usage: ResponseUsage {
            input_tokens: 0,
            output_tokens: 0,
        },
    }
}

/// 将纯文本切分为多个 delta，构造模拟流式 Anthropic SSE 响应。
///
/// - `model` / `message_id`：写入事件的模型名与消息 ID；
/// - `text`：完整回复文本，按字符（非字节）切片；
/// - `chunk_chars`：每个 `text_delta` 的字符数。
///
/// 事件序列为 message_start、content_block_start、若干 content_block_delta、
/// content_block_stop、message_delta（`end_turn`）与 message_stop。
/// 返回一次性携带完整 `text/event-stream` 响应体的 axum `Response`
/// （不做逐事件真实推送）；响应体构建失败时返回 `Err`（`AppError::Internal`）。
pub fn build_anthropic_stream_from_plain(
    model: &str,
    message_id: &str,
    text: &str,
    chunk_chars: usize,
) -> Result<Response, AppError> {
    let mut events: Vec<String> = Vec::new();

    // message_start event
    let start_event = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": message_id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        }
    });
    events.push(format!(
        "event: message_start\ndata: {}\n\n",
        serde_json::to_string(&start_event).unwrap_or_default()
    ));

    // content_block_start event
    let block_start = serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "text",
            "text": ""
        }
    });
    events.push(format!(
        "event: content_block_start\ndata: {}\n\n",
        serde_json::to_string(&block_start).unwrap_or_default()
    ));

    // content_block_delta events
    let chars: Vec<char> = text.chars().collect();
    for i in (0..chars.len()).step_by(chunk_chars) {
        let end = (i + chunk_chars).min(chars.len());
        let content: String = chars[i..end].iter().collect();

        let delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": content
            }
        });
        events.push(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&delta).unwrap_or_default()
        ));
    }

    // content_block_stop event
    let block_stop = serde_json::json!({
        "type": "content_block_stop",
        "index": 0
    });
    events.push(format!(
        "event: content_block_stop\ndata: {}\n\n",
        serde_json::to_string(&block_stop).unwrap_or_default()
    ));

    // message_delta event
    let msg_delta = serde_json::json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": "end_turn",
            "stop_sequence": null
        },
        "usage": {
            "output_tokens": 0
        }
    });
    events.push(format!(
        "event: message_delta\ndata: {}\n\n",
        serde_json::to_string(&msg_delta).unwrap_or_default()
    ));

    // message_stop event
    events.push("event: message_stop\ndata: {}\n\n".to_string());

    let body = events.join("");
    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::from(body))
        .map_err(|e| AppError::Internal(e.to_string()))?)
}

/// 构造非流式 OpenAI Responses API 响应。
///
/// - `model` / `response_id`：响应中的模型名与响应 ID；
/// - `text`：完整回复文本。
///
/// 返回单个 message 输出项（状态 `completed`、内容类型 `output_text`）、
/// usage 为零值的响应体。
pub fn build_responses_response_from_plain(
    model: &str,
    response_id: &str,
    text: &str,
) -> ResponsesResponse {
    ResponsesResponse {
        id: response_id.to_string(),
        object: "response".to_string(),
        created_at: now_unix_secs(),
        model: model.to_string(),
        output: vec![ResponseOutputItem {
            item_type: "message".to_string(),
            id: format!("msg_{}", now_unix_secs()),
            call_id: None,
            status: "completed".to_string(),
            role: "assistant".to_string(),
            name: None,
            arguments: None,
            content: vec![ResponseContentPart {
                content_type: "output_text".to_string(),
                text: text.to_string(),
                annotations: vec![],
            }],
        }],
        usage: OpenAIResponseUsage {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        },
    }
}

/// 将纯文本切分为多个 delta，构造模拟流式 OpenAI Responses SSE 响应。
///
/// - `model` / `response_id`：写入事件的模型名与响应 ID；
/// - `text`：完整回复文本，按字符（非字节）切片；
/// - `chunk_chars`：每个 `response.output_text.delta` 的字符数。
///
/// 事件序列为 response.created、response.output_item.added、若干
/// response.output_text.delta、response.output_text.done、
/// response.output_item.done 与 response.completed。
/// 返回一次性携带完整 `text/event-stream` 响应体的 axum `Response`
/// （不做逐事件真实推送）；响应体构建失败时返回 `Err`（`AppError::Internal`）。
pub fn build_responses_stream_from_plain(
    model: &str,
    response_id: &str,
    text: &str,
    chunk_chars: usize,
) -> Result<Response, AppError> {
    let mut events: Vec<String> = Vec::new();

    // response.created event
    let created_event = serde_json::json!({
        "type": "response.created",
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": now_unix_secs(),
            "model": model,
            "status": "in_progress"
        }
    });
    events.push(format!(
        "event: response.created\ndata: {}\n\n",
        serde_json::to_string(&created_event).unwrap_or_default()
    ));

    // response.output_item.added event
    let output_added = serde_json::json!({
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {
            "type": "message",
            "id": format!("msg_{}", now_unix_secs()),
            "status": "in_progress",
            "role": "assistant",
            "content": []
        }
    });
    events.push(format!(
        "event: response.output_item.added\ndata: {}\n\n",
        serde_json::to_string(&output_added).unwrap_or_default()
    ));

    // response.output_text.delta events
    let chars: Vec<char> = text.chars().collect();
    for i in (0..chars.len()).step_by(chunk_chars) {
        let end = (i + chunk_chars).min(chars.len());
        let content: String = chars[i..end].iter().collect();

        let delta = serde_json::json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": content
        });
        events.push(format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            serde_json::to_string(&delta).unwrap_or_default()
        ));
    }

    // response.output_text.done event
    let text_done = serde_json::json!({
        "type": "response.output_text.done",
        "output_index": 0,
        "content_index": 0,
        "text": text
    });
    events.push(format!(
        "event: response.output_text.done\ndata: {}\n\n",
        serde_json::to_string(&text_done).unwrap_or_default()
    ));

    // response.output_item.done event
    let item_done = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "type": "message",
            "id": format!("msg_{}", now_unix_secs()),
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text
            }]
        }
    });
    events.push(format!(
        "event: response.output_item.done\ndata: {}\n\n",
        serde_json::to_string(&item_done).unwrap_or_default()
    ));

    // response.completed event
    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": now_unix_secs(),
            "model": model,
            "status": "completed",
            "output": [{
                "type": "message",
                "id": format!("msg_{}", now_unix_secs()),
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": text
                }]
            }],
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "total_tokens": 0
            }
        }
    });
    events.push(format!(
        "event: response.completed\ndata: {}\n\n",
        serde_json::to_string(&completed).unwrap_or_default()
    ));

    let body = events.join("");
    Ok(Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::from(body))
        .map_err(|e| AppError::Internal(e.to_string()))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_text_response() {
        let resp = build_openai_text_response_from_plain("gpt-test", "chatcmpl-1", "Hello world");
        assert_eq!(resp.id, "chatcmpl-1");
        assert_eq!(resp.model, "gpt-test");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Hello world")
        );
        assert_eq!(resp.choices[0].finish_reason, "stop");
        assert_eq!(resp.usage.total_tokens, 0);
    }

    #[test]
    fn test_openai_stream_response_structure() {
        let result = build_openai_text_stream_from_plain("gpt-test", "chatcmpl-1", "Hello", 2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_openai_stream_chunking() {
        let text = "Hello world test";
        let result = build_openai_text_stream_from_plain("gpt-test", "chatcmpl-1", text, 5);
        assert!(result.is_ok());
    }

    #[test]
    fn test_anthropic_message_response() {
        let resp = build_anthropic_message_from_plain("claude-3", "msg_1", "Hello world");
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.model, "claude-3");
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ResponseContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected text block"),
        }
        assert_eq!(resp.stop_reason, "end_turn");
    }

    #[test]
    fn test_anthropic_stream_response_structure() {
        let result = build_anthropic_stream_from_plain("claude-3", "msg_1", "Hello", 2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_responses_response() {
        let resp = build_responses_response_from_plain("gpt-test", "resp_1", "Hello world");
        assert_eq!(resp.id, "resp_1");
        assert_eq!(resp.model, "gpt-test");
        assert_eq!(resp.output.len(), 1);
        assert_eq!(resp.output[0].content.len(), 1);
        assert_eq!(resp.output[0].content[0].text, "Hello world");
        assert_eq!(resp.usage.total_tokens, 0);
    }

    #[test]
    fn test_responses_stream_response_structure() {
        let result = build_responses_stream_from_plain("gpt-test", "resp_1", "Hello", 2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_text_response() {
        let resp = build_openai_text_response_from_plain("gpt-test", "chatcmpl-1", "");
        assert_eq!(resp.choices[0].message.content.as_deref(), Some(""));
    }

    #[test]
    fn test_long_text_chunking() {
        let text = "a".repeat(100);
        let result = build_openai_text_stream_from_plain("gpt-test", "chatcmpl-1", &text, 10);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unicode_text_chunking() {
        let text = "你好世界测试文本";
        let result = build_openai_text_stream_from_plain("gpt-test", "chatcmpl-1", text, 2);
        assert!(result.is_ok());
    }
}
