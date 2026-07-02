//! `POST /v1/responses` 与 `/openai/v1/responses` 非流式响应的 JSON 构造。
//! 输出符合 OpenAI Responses API 规范的 `ResponsesResponse` 结构。

use crate::model::openai::{
    ResponseContentPart, ResponseOutputItem, ResponsesResponse, ResponseUsage,
};
use crate::tool_runtime::openai_compiler::CompiledToolCall;
use crate::tool_runtime::pipeline::PipelineOutput;

/// 从 pipeline 输出构造非流式 Responses 响应。
pub fn build_responses_response_from_pipeline(
    task_id: &str,
    model: &str,
    output: &PipelineOutput,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> ResponsesResponse {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let items = match output {
        PipelineOutput::ToolCalls { calls, .. } => build_function_call_items(calls),
        PipelineOutput::FinalAnswer { text, .. } => build_message_item(text),
    };

    ResponsesResponse {
        id: task_id.to_string(),
        object: "response".to_string(),
        created_at,
        model: model.to_string(),
        output: items,
        usage: ResponseUsage {
            input_tokens: prompt_tokens,
            output_tokens: completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    }
}

fn build_function_call_items(calls: &[CompiledToolCall]) -> Vec<ResponseOutputItem> {
    calls
        .iter()
        .map(|call| ResponseOutputItem {
            item_type: "function_call".to_string(),
            id: format!("fc_{}", call.id),
            call_id: Some(call.id.clone()),
            status: "completed".to_string(),
            role: "assistant".to_string(),
            name: Some(call.name.clone()),
            arguments: Some(call.input.to_string()),
            content: Vec::new(),
        })
        .collect()
}

fn build_message_item(text: &str) -> Vec<ResponseOutputItem> {
    vec![ResponseOutputItem {
        item_type: "message".to_string(),
        id: format!("msg_{}", crate::tool_runtime::uuid_short()),
        call_id: None,
        status: "completed".to_string(),
        role: "assistant".to_string(),
        name: None,
        arguments: None,
        content: vec![ResponseContentPart {
            content_type: "output_text".to_string(),
            text: text.to_string(),
            annotations: Vec::new(),
        }],
    }]
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

    #[test]
    fn final_answer_output_is_single_message_item() {
        let out = PipelineOutput::FinalAnswer {
            text: "hello".into(),
            meta: meta_dummy(),
        };
        let resp = build_responses_response_from_pipeline("resp_1", "gpt-test", &out, 5, 2);
        assert_eq!(resp.output.len(), 1);
        assert_eq!(resp.output[0].item_type, "message");
        assert_eq!(resp.output[0].content[0].text, "hello");
        assert!(resp.output[0].call_id.is_none());
        assert!(resp.output[0].name.is_none());
        assert!(resp.output[0].arguments.is_none());
    }

    #[test]
    fn tool_call_output_contains_one_item_per_call_with_arguments() {
        let calls = vec![
            CompiledToolCall {
                id: "c1".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            CompiledToolCall {
                id: "c2".into(),
                name: "Read".into(),
                input: serde_json::json!({"path": "/tmp"}),
            },
        ];
        let out = PipelineOutput::ToolCalls {
            calls,
            meta: meta_dummy(),
        };
        let resp = build_responses_response_from_pipeline("resp_1", "gpt-test", &out, 10, 5);
        assert_eq!(resp.output.len(), 2);
        for item in &resp.output {
            assert_eq!(item.item_type, "function_call");
            assert_eq!(item.status, "completed");
            assert!(item.call_id.is_some());
            assert!(item.name.is_some());
            assert!(item.arguments.is_some());
            assert!(item.content.is_empty());
        }
        assert_eq!(resp.output[0].name.as_deref(), Some("Bash"));
        assert_eq!(resp.output[1].name.as_deref(), Some("Read"));
        assert_eq!(resp.output[0].call_id.as_deref(), Some("c1"));
        assert_eq!(resp.output[1].call_id.as_deref(), Some("c2"));
    }

    #[test]
    fn usage_tokens_round_trip_into_response_usage() {
        let out = PipelineOutput::FinalAnswer {
            text: "x".into(),
            meta: meta_dummy(),
        };
        let resp = build_responses_response_from_pipeline("r", "m", &out, 7, 3);
        assert_eq!(resp.usage.input_tokens, 7);
        assert_eq!(resp.usage.output_tokens, 3);
        assert_eq!(resp.usage.total_tokens, 10);
    }

    #[test]
    fn response_object_id_and_model_set_from_args() {
        let out = PipelineOutput::FinalAnswer {
            text: "x".into(),
            meta: meta_dummy(),
        };
        let resp = build_responses_response_from_pipeline("resp_xyz", "claude-test", &out, 0, 0);
        assert_eq!(resp.id, "resp_xyz");
        assert_eq!(resp.model, "claude-test");
        assert_eq!(resp.object, "response");
    }
}
