//! 工具调用决策 + ledger + stats 流水线的统一入口。
//! 供 `handler::chat` 与 `handler::responses` 共用。

use std::time::Instant;

use crate::AppState;
use crate::model::openai::ChatCompletionRequest;
use crate::tool_runtime::openai_compiler::CompiledToolCall;

#[derive(Debug, Clone)]
pub struct PipelineMeta {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub platform_key: String,
    pub short_model: String,
}

#[derive(Debug, Clone)]
pub enum PipelineOutput {
    ToolCalls {
        calls: Vec<CompiledToolCall>,
        meta: PipelineMeta,
    },
    FinalAnswer {
        text: String,
        meta: PipelineMeta,
    },
}

pub async fn run_openai_tool_pipeline(
    _state: &AppState,
    _request: &ChatCompletionRequest,
    _upstream_output: String,
    _platform_key: &str,
    _start_time: Instant,
) -> PipelineOutput {
    unimplemented!("pipeline 尚未实现")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_meta_carries_compiler_usage_and_platform_key() {
        let meta = PipelineMeta {
            provider: "openai-compatible".to_string(),
            model: "gpt-tool".to_string(),
            base_url: Some("http://tool.local/v1".to_string()),
            prompt_tokens: 11,
            completion_tokens: 3,
            platform_key: "ouyi8".to_string(),
            short_model: "gpt-test".to_string(),
        };
        assert_eq!(meta.prompt_tokens, 11);
        assert_eq!(meta.completion_tokens, 3);
        assert_eq!(meta.platform_key, "ouyi8");
    }

    #[test]
    fn pipeline_output_tool_calls_carries_calls_and_meta() {
        let calls = vec![CompiledToolCall {
            id: "call_1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }];
        let out = PipelineOutput::ToolCalls {
            calls: calls.clone(),
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
        match out {
            PipelineOutput::ToolCalls { calls, .. } => {
                assert_eq!(calls[0].name, "Bash");
            }
            _ => panic!("expected ToolCalls"),
        }
    }
}
