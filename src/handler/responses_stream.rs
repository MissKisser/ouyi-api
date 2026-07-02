//! `POST /v1/responses` 与 `/openai/v1/responses` 流式响应的事件序列化。
//! 按 OpenAI Responses API 规范产出 `response.created` / `response.output_item.added` /
//! `response.function_call_arguments.delta` / `response.completed` 等事件。

use axum::response::sse::Event;
use serde_json::json;

use crate::tool_runtime::openai_compiler::CompiledToolCall;
use crate::tool_runtime::pipeline::PipelineOutput;

/// 构造一个 SSE 事件
fn sse_event(event_name: &str, data: serde_json::Value) -> Event {
    Event::default().event(event_name).data(data.to_string())
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 构造文本 message 路径的全部事件序列。
/// text_deltas 已是拼接好的文本（不切分），用于单段输出。
pub fn build_final_answer_events(task_id: &str, model: &str, text: &str) -> Vec<Event> {
    let created = now_unix_secs();
    let msg_id = format!("msg_{}", crate::tool_runtime::uuid_short());

    let mut events = Vec::new();
    events.push(sse_event(
        "response.created",
        json!({
            "type": "response.created",
            "response": {
                "id": task_id,
                "object": "response",
                "created_at": created,
                "model": model,
                "output": [],
                "status": "in_progress",
            },
        }),
    ));
    events.push(sse_event(
        "response.in_progress",
        json!({
            "type": "response.in_progress",
            "response": { "id": task_id, "status": "in_progress" },
        }),
    ));
    events.push(sse_event(
        "response.output_item.added",
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": msg_id,
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            },
        }),
    ));
    events.push(sse_event(
        "response.content_part.added",
        json!({
            "type": "response.content_part.added",
            "item_id": msg_id,
            "output_index": 0,
            "content_index": 0,
            "part": { "type": "output_text", "text": "", "annotations": [] },
        }),
    ));
    if !text.is_empty() {
        events.push(sse_event(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": msg_id,
                "output_index": 0,
                "content_index": 0,
                "delta": text,
            }),
        ));
    }
    events.push(sse_event(
        "response.output_text.done",
        json!({
            "type": "response.output_text.done",
            "item_id": msg_id,
            "output_index": 0,
            "content_index": 0,
            "text": text,
        }),
    ));
    events.push(sse_event(
        "response.content_part.done",
        json!({
            "type": "response.content_part.done",
            "item_id": msg_id,
            "output_index": 0,
            "content_index": 0,
            "part": { "type": "output_text", "text": text, "annotations": [] },
        }),
    ));
    events.push(sse_event(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": msg_id,
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                }],
            },
        }),
    ));
    events.push(sse_event(
        "response.completed",
        json!({
            "type": "response.completed",
            "response": {
                "id": task_id,
                "object": "response",
                "created_at": created,
                "model": model,
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": msg_id,
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    }],
                }],
            },
        }),
    ));
    events
}

/// 构造 function_call 路径的全部事件序列。每个 call 一组 added → deltas → done → output_item.done。
pub fn build_tool_call_events(task_id: &str, model: &str, calls: &[CompiledToolCall]) -> Vec<Event> {
    let created = now_unix_secs();

    let mut events = Vec::new();
    events.push(sse_event(
        "response.created",
        json!({
            "type": "response.created",
            "response": {
                "id": task_id,
                "object": "response",
                "created_at": created,
                "model": model,
                "output": [],
                "status": "in_progress",
            },
        }),
    ));
    events.push(sse_event(
        "response.in_progress",
        json!({
            "type": "response.in_progress",
            "response": { "id": task_id, "status": "in_progress" },
        }),
    ));

    for (idx, call) in calls.iter().enumerate() {
        let args_str = call.input.to_string();
        let call_id = &call.id;
        let output_index = idx as u32;

        events.push(sse_event(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "type": "function_call",
                    "id": format!("fc_{call_id}"),
                    "call_id": call_id,
                    "name": call.name,
                    "arguments": "",
                    "status": "in_progress",
                },
            }),
        ));
        // 简化：整段 arguments 作为单个 delta
        if !args_str.is_empty() {
            events.push(sse_event(
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": format!("fc_{call_id}"),
                    "output_index": output_index,
                    "delta": args_str,
                }),
            ));
        }
        events.push(sse_event(
            "response.function_call_arguments.done",
            json!({
                "type": "response.function_call_arguments.done",
                "item_id": format!("fc_{call_id}"),
                "output_index": output_index,
                "arguments": args_str,
            }),
        ));
        events.push(sse_event(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": {
                    "type": "function_call",
                    "id": format!("fc_{call_id}"),
                    "call_id": call_id,
                    "name": call.name,
                    "arguments": args_str,
                    "status": "completed",
                },
            }),
        ));
    }

    // 复制一个最终 output 数组用于 response.completed
    let output_items: Vec<serde_json::Value> = calls
        .iter()
        .enumerate()
        .map(|(idx, call)| {
            json!({
                "type": "function_call",
                "id": format!("fc_{}", call.id),
                "call_id": call.id,
                "name": call.name,
                "arguments": call.input.to_string(),
                "status": "completed",
                "output_index": idx,
            })
        })
        .collect();

    events.push(sse_event(
        "response.completed",
        json!({
            "type": "response.completed",
            "response": {
                "id": task_id,
                "object": "response",
                "created_at": created,
                "model": model,
                "status": "completed",
                "output": output_items,
            },
        }),
    ));
    events
}

/// 把 PipelineOutput 翻译为 SSE 事件序列。
pub fn events_for_pipeline_output(
    task_id: &str,
    model: &str,
    output: &PipelineOutput,
) -> Vec<Event> {
    match output {
        PipelineOutput::ToolCalls { calls, .. } => build_tool_call_events(task_id, model, calls),
        PipelineOutput::FinalAnswer { text, .. } => build_final_answer_events(task_id, model, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_runtime::pipeline::PipelineMeta;

    fn meta_dummy() -> PipelineMeta {
        PipelineMeta {
            provider: "openai-compatible".into(),
            model: "gpt-tool".into(),
            base_url: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            platform_key: "ouyi8".into(),
            short_model: "gpt-test".into(),
        }
    }

    /// 从一个 Event 反解出 data 字段对应的 JSON。
    /// axum 0.8 的 `Event` 没有公开的 `data()` 访问器，因此借道 `Sse` 单元素流的 HTTP body
    /// 拿到渲染后的字节再解析。该函数仅在测试中使用，保证对生产路径无影响。
    fn event_data_json(ev: &Event) -> serde_json::Value {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;
        use axum::response::sse::Sse;
        use futures::stream;

        let owned = ev.clone();
        let sse = Sse::new(stream::once(async move { Ok::<_, std::convert::Infallible>(owned) }));
        let resp = sse.into_response();
        let bytes = futures::executor::block_on(async {
            to_bytes(resp.into_body(), 64 * 1024).await
        })
        .expect("读取 SSE body 应成功");
        let text = std::str::from_utf8(&bytes).expect("SSE body 应为 UTF-8");
        let mut data_lines: Vec<&str> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                data_lines.push(rest);
            }
        }
        let joined = data_lines.join("\n");
        serde_json::from_str(&joined).expect("data 行应为合法 JSON")
    }

    /// 提取所有 SSE 事件名为有序 Vec
    fn event_names(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .map(|e| {
                let v = event_data_json(e);
                v.get("type")
                    .and_then(|t| t.as_str())
                    .map(String::from)
                    .unwrap_or_default()
            })
            .collect()
    }

    #[test]
    fn final_answer_event_count_matches_spec() {
        let events = build_final_answer_events("resp_1", "gpt-test", "hello");
        // 期望：created + in_progress + output_item.added + content_part.added
        //       + output_text.delta + output_text.done + content_part.done
        //       + output_item.done + response.completed = 9
        assert_eq!(events.len(), 9);
        let names = event_names(&events);
        assert_eq!(
            names,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
    }

    #[test]
    fn final_answer_event_count_when_empty_text_skips_delta() {
        let events = build_final_answer_events("resp_1", "gpt-test", "");
        // 期望：与上面 9 个事件相比，output_text.delta 被跳过 = 8
        assert_eq!(events.len(), 8);
        let names = event_names(&events);
        assert!(!names.contains(&"response.output_text.delta".to_string()));
        assert!(names.contains(&"response.output_text.done".to_string()));
    }

    #[test]
    fn tool_call_event_count_single_call() {
        let calls = vec![CompiledToolCall {
            id: "call_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
        }];
        let events = build_tool_call_events("resp_1", "gpt-test", &calls);
        // 期望：created + in_progress + (added + delta + done + output_item.done) + completed
        // = 2 + 4 + 1 = 7
        assert_eq!(events.len(), 7);
    }

    #[test]
    fn tool_call_event_count_multiple_calls() {
        let calls = vec![
            CompiledToolCall {
                id: "call_1".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            CompiledToolCall {
                id: "call_2".into(),
                name: "Read".into(),
                input: serde_json::json!({"path": "/tmp"}),
            },
        ];
        let events = build_tool_call_events("resp_1", "gpt-test", &calls);
        // 2 个 call：created + in_progress + 2*(added+delta+done+output_item.done) + completed
        // = 2 + 2*4 + 1 = 11
        assert_eq!(events.len(), 11);
    }

    #[test]
    fn tool_call_event_payload_contains_call_id_and_name() {
        let calls = vec![CompiledToolCall {
            id: "call_abc".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "pwd"}),
        }];
        let events = build_tool_call_events("resp_1", "gpt-test", &calls);
        // 找到 output_item.added 事件，data 应含 call_id 与 name
        let added_payload: Vec<String> = events
            .iter()
            .filter_map(|e| {
                let v = event_data_json(e);
                if v.get("type").and_then(|t| t.as_str()) == Some("response.output_item.added") {
                    Some(v.to_string())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(added_payload.len(), 1);
        assert!(added_payload[0].contains("call_abc"));
        assert!(added_payload[0].contains("Bash"));
    }

    #[test]
    fn events_for_pipeline_output_dispatches_correctly() {
        let calls = vec![CompiledToolCall {
            id: "c1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
        }];
        let tool_out = PipelineOutput::ToolCalls {
            calls,
            meta: meta_dummy(),
        };
        let tool_events = events_for_pipeline_output("resp_1", "gpt-test", &tool_out);
        assert_eq!(tool_events.len(), 7);

        let text_out = PipelineOutput::FinalAnswer {
            text: "hi".into(),
            meta: meta_dummy(),
        };
        let text_events = events_for_pipeline_output("resp_1", "gpt-test", &text_out);
        assert_eq!(text_events.len(), 9);
    }
}