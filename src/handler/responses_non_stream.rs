//! `POST /v1/responses` 与 `/openai/v1/responses` 非流式响应的 JSON 构造。
//! 输出符合 OpenAI Responses API 规范的 `ResponsesResponse` 结构。

use crate::model::openai::{
    ResponseContentPart, ResponseOutputItem, ResponseUsage, ResponsesResponse,
};
use crate::tool_runtime::openai_compiler::CompiledToolCall;
use crate::tool_runtime::pipeline::PipelineOutput;

/// 从 pipeline 输出构造非流式 Responses 响应。
/// `usage_override` 在调用方已知 Ouyi 上游 token 计数时传入；
/// 传 `None` 时使用 pipeline meta 中的工具模型 usage 作为兜底。
pub fn build_responses_response_from_pipeline(
    task_id: &str,
    model: &str,
    output: &PipelineOutput,
    usage_override: Option<(u64, u64)>,
) -> ResponsesResponse {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let items = match output {
        PipelineOutput::ToolCalls { calls, .. } => build_function_call_items(calls),
        PipelineOutput::FinalAnswer { text, .. } => build_message_item(text),
    };

    let (input_tokens, output_tokens) = usage_override.unwrap_or_else(|| {
        let meta = match output {
            PipelineOutput::ToolCalls { meta, .. } | PipelineOutput::FinalAnswer { meta, .. } => {
                meta
            }
        };
        (meta.prompt_tokens, meta.completion_tokens)
    });

    ResponsesResponse {
        id: task_id.to_string(),
        object: "response".to_string(),
        created_at,
        model: model.to_string(),
        output: items,
        usage: ResponseUsage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
        },
    }
}

/// 把编译后的工具调用列表映射为 `function_call` 类型的输出项数组，
/// 每个调用一项，`arguments` 为序列化后的 JSON 字符串。
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

/// 把最终回答文本包装为单个 `message` 类型输出项，
/// 内容为 `output_text` 内容块，不携带注解。
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
        let resp = build_responses_response_from_pipeline("resp_1", "gpt-test", &out, Some((5, 2)));
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
        let resp =
            build_responses_response_from_pipeline("resp_1", "gpt-test", &out, Some((10, 5)));
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
    fn usage_override_takes_precedence_over_meta() {
        let out = PipelineOutput::FinalAnswer {
            text: "x".into(),
            meta: PipelineMeta {
                provider: "x".into(),
                model: "y".into(),
                base_url: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                platform_key: "ouyi8".into(),
                short_model: "gpt-test".into(),
            },
        };
        let resp = build_responses_response_from_pipeline("r", "m", &out, Some((7, 3)));
        assert_eq!(resp.usage.input_tokens, 7);
        assert_eq!(resp.usage.output_tokens, 3);
        assert_eq!(resp.usage.total_tokens, 10);
    }

    #[test]
    fn usage_falls_back_to_meta_when_no_override() {
        let out = PipelineOutput::FinalAnswer {
            text: "x".into(),
            meta: PipelineMeta {
                provider: "x".into(),
                model: "y".into(),
                base_url: None,
                prompt_tokens: 11,
                completion_tokens: 4,
                platform_key: "ouyi8".into(),
                short_model: "gpt-test".into(),
            },
        };
        let resp = build_responses_response_from_pipeline("r", "m", &out, None);
        assert_eq!(resp.usage.input_tokens, 11);
        assert_eq!(resp.usage.output_tokens, 4);
        assert_eq!(resp.usage.total_tokens, 15);
    }

    #[test]
    fn response_object_id_and_model_set_from_args() {
        let out = PipelineOutput::FinalAnswer {
            text: "x".into(),
            meta: meta_dummy(),
        };
        let resp = build_responses_response_from_pipeline("resp_xyz", "claude-test", &out, None);
        assert_eq!(resp.id, "resp_xyz");
        assert_eq!(resp.model, "claude-test");
        assert_eq!(resp.object, "response");
    }
}
