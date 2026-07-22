use crate::adapter::tool_call::{ToolCallParser, tool_delta_to_full_call};
use crate::client::sse::OuyiEventType;
use crate::model::openai::*;
use crate::model::ouyi::OuyiSseEvent;

pub struct StreamConverter {
    model: String,
    task_id: String,
    tool_parser: ToolCallParser,
    created: u64,
    first_chunk: bool,
    prompt_tokens: u64,
    completion_tokens: u64,
}

impl StreamConverter {
    pub fn new(model: String, task_id: String) -> Self {
        Self {
            model,
            task_id,
            tool_parser: ToolCallParser::new(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            first_chunk: true,
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    pub fn convert_event(&mut self, event: &OuyiSseEvent) -> Option<Vec<ChatCompletionChunk>> {
        let event_type = OuyiEventType::from(event.event_type.as_str());

        match event_type {
            OuyiEventType::StringChunk => {
                let text = event.data.as_ref()?.as_str().unwrap_or("");
                if text.is_empty() {
                    return None;
                }
                let parsed = self.tool_parser.process_chunk(text);
                self.build_chunks_from_parsed(parsed)
            }
            OuyiEventType::Object => {
                if let Some(data) = &event.data
                    && let Ok(usage) =
                        serde_json::from_value::<crate::model::ouyi::OuyiUsage>(data.clone())
                {
                    self.prompt_tokens = usage.prompt_tokens;
                    self.completion_tokens = usage.completion_tokens;
                    tracing::debug!(
                        prompt = usage.prompt_tokens,
                        completion = usage.completion_tokens,
                        "StreamConverter 从 object 事件捕获 token 用量"
                    );
                }
                None
            }
            OuyiEventType::Done => {
                if let Some(data) = &event.data {
                    if let Ok(usage) =
                        serde_json::from_value::<crate::model::ouyi::OuyiUsage>(data.clone())
                    {
                        self.prompt_tokens = usage.prompt_tokens;
                        self.completion_tokens = usage.completion_tokens;
                        tracing::debug!(
                            prompt = usage.prompt_tokens,
                            completion = usage.completion_tokens,
                            "StreamConverter 从 done 事件捕获 token 用量"
                        );
                    }
                }
                let finish_reason = if self.tool_parser.is_in_tool_call() {
                    "tool_calls"
                } else {
                    "stop"
                };
                Some(vec![self.make_chunk(
                    ChunkDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                    },
                    Some(finish_reason.to_string()),
                )])
            }
            OuyiEventType::Unknown(_) => None,
        }
    }

    pub fn tokens(&self) -> (u64, u64) {
        (self.prompt_tokens, self.completion_tokens)
    }

    fn build_chunks_from_parsed(
        &mut self,
        parsed: crate::adapter::tool_call::ParsedOutput,
    ) -> Option<Vec<ChatCompletionChunk>> {
        let mut chunks = Vec::new();

        if !parsed.normal_text.is_empty() {
            let role = if self.first_chunk {
                self.first_chunk = false;
                Some("assistant".to_string())
            } else {
                None
            };
            chunks.push(self.make_chunk(
                ChunkDelta {
                    role,
                    content: Some(parsed.normal_text),
                    tool_calls: None,
                },
                None,
            ));
        }

        for delta in parsed.tool_deltas {
            let role = if self.first_chunk {
                self.first_chunk = false;
                Some("assistant".to_string())
            } else {
                None
            };
            chunks.push(self.make_chunk(
                ChunkDelta {
                    role,
                    content: None,
                    tool_calls: Some(vec![delta]),
                },
                None,
            ));
        }

        if chunks.is_empty() {
            None
        } else {
            Some(chunks)
        }
    }

    fn make_chunk(&self, delta: ChunkDelta, finish_reason: Option<String>) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: self.task_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: self.created,
            model: self.model.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta,
                finish_reason,
            }],
            usage: None,
        }
    }
}

pub struct SseCollector {
    content_parts: Vec<String>,
    usage: Option<crate::model::ouyi::OuyiUsage>,
    model: String,
    task_id: String,
    tool_parser: ToolCallParser,
    created: u64,
}

impl SseCollector {
    pub fn new(model: String, task_id: String) -> Self {
        Self {
            content_parts: Vec::new(),
            usage: None,
            model,
            task_id,
            tool_parser: ToolCallParser::new(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    pub fn push_event(&mut self, event: &OuyiSseEvent) {
        let event_type = OuyiEventType::from(event.event_type.as_str());
        match event_type {
            OuyiEventType::StringChunk => {
                if let Some(data) = &event.data
                    && let Some(text) = data.as_str()
                    && !text.is_empty()
                {
                    self.content_parts.push(text.to_string());
                }
            }
            OuyiEventType::Object => {
                if let Some(data) = &event.data
                    && let Ok(ouyi_usage) =
                        serde_json::from_value::<crate::model::ouyi::OuyiUsage>(data.clone())
                {
                    self.usage = Some(ouyi_usage);
                }
            }
            _ => {}
        }
    }

    pub fn tokens(&self) -> (u64, u64) {
        match &self.usage {
            Some(u) => (u.prompt_tokens, u.completion_tokens),
            None => (0, 0),
        }
    }

    pub fn text_content(&self) -> String {
        self.content_parts.join("")
    }

    pub fn build_response(mut self) -> ChatCompletionResponse {
        let full_text: String = self.content_parts.join("");
        let parsed = self.tool_parser.process_chunk(&full_text);

        let (content, tool_calls, finish_reason) = if parsed.tool_deltas.is_empty() {
            (Some(full_text), None, "stop")
        } else {
            let calls: Vec<ToolCall> = parsed
                .tool_deltas
                .iter()
                .map(tool_delta_to_full_call)
                .collect();
            (None, Some(calls), "tool_calls")
        };

        let usage = match &self.usage {
            Some(u) => Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.prompt_tokens + u.completion_tokens,
            },
            None => Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
        };

        ChatCompletionResponse {
            id: self.task_id,
            object: "chat.completion".to_string(),
            created: self.created,
            model: self.model,
            choices: vec![ResponseChoice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".to_string(),
                    content,
                    tool_calls,
                },
                finish_reason: finish_reason.to_string(),
            }],
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_string_event(text: &str) -> OuyiSseEvent {
        OuyiSseEvent {
            id: Some("1".to_string()),
            event_type: "string".to_string(),
            data: Some(json!(text)),
            err: None,
            code: None,
        }
    }

    fn make_done_event() -> OuyiSseEvent {
        OuyiSseEvent {
            id: Some("1".to_string()),
            event_type: "done".to_string(),
            data: None,
            err: None,
            code: None,
        }
    }

    #[test]
    fn test_stream_converter_text() {
        let mut conv = StreamConverter::new("test-model".to_string(), "chatcmpl-1".to_string());
        let event = make_string_event("Hello");
        let chunks = conv.convert_event(&event).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_stream_converter_done() {
        let mut conv = StreamConverter::new("test-model".to_string(), "chatcmpl-1".to_string());
        let event = make_done_event();
        let chunks = conv.convert_event(&event).unwrap();
        assert_eq!(chunks[0].choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn test_sse_collector_basic() {
        let mut collector = SseCollector::new("test-model".to_string(), "chatcmpl-1".to_string());
        collector.push_event(&make_string_event("Hello "));
        collector.push_event(&make_string_event("world"));
        collector.push_event(&make_done_event());

        let resp = collector.build_response();
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Hello world")
        );
        assert_eq!(resp.choices[0].finish_reason, "stop");
    }

    #[test]
    fn test_sse_collector_usage_fallback() {
        let collector = SseCollector::new("test-model".to_string(), "chatcmpl-1".to_string());
        let resp = collector.build_response();
        assert_eq!(resp.usage.prompt_tokens, 0);
        assert_eq!(resp.usage.completion_tokens, 0);
    }
}
