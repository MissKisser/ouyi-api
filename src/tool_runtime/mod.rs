use std::time::Instant;

use dashmap::DashMap;

use crate::error::AppError;
use crate::model::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};

pub mod broker;
pub mod openai_compiler;

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", ts % 0xFFFFFF)
}

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub tool_use_id: String,
    pub tool_name: String,
    pub created_at: Instant,
    pub long_task_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingLongTask {
    pub task_id: String,
    pub file_path: String,
    pub done_marker: String,
    pub polls: u32,
    pub max_polls: u32,
    pub tool_call_keys: Vec<String>,
}

#[derive(Default)]
pub struct ToolLedger {
    pending: DashMap<String, PendingToolCall>,
    long_tasks: DashMap<String, PendingLongTask>,
}

impl ToolLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_tool_use(
        &self,
        session_key: &str,
        tool_use_id: &str,
        tool_name: &str,
    ) -> Result<(), AppError> {
        if session_key.is_empty() || tool_use_id.is_empty() || tool_name.is_empty() {
            return Err(AppError::BadRequest(
                "工具会话账本记录缺少必要字段".to_string(),
            ));
        }
        self.pending.insert(
            session_key.to_string(),
            PendingToolCall {
                tool_use_id: tool_use_id.to_string(),
                tool_name: tool_name.to_string(),
                created_at: Instant::now(),
                long_task_id: None,
            },
        );
        Ok(())
    }

    #[cfg(test)]
    pub fn validate_tool_result(
        &self,
        session_key: &str,
        tool_use_id: &str,
    ) -> Result<(), AppError> {
        match self.pending.get(session_key) {
            Some(pending) if pending.tool_use_id == tool_use_id => Ok(()),
            Some(pending) => Err(AppError::BadRequest(format!(
                "tool_result id 不匹配: 期望 {}, 收到 {}",
                pending.tool_use_id, tool_use_id
            ))),
            None => Err(AppError::BadRequest(format!(
                "未找到等待中的工具调用会话: {}",
                session_key
            ))),
        }
    }

    #[cfg(test)]
    pub fn complete_tool_result(
        &self,
        session_key: &str,
        tool_use_id: &str,
    ) -> Result<(), AppError> {
        self.validate_tool_result(session_key, tool_use_id)?;
        self.pending.remove(session_key);
        Ok(())
    }

    #[cfg(test)]
    pub fn pending_tool_name(&self, session_key: &str) -> Option<String> {
        self.pending
            .get(session_key)
            .map(|pending| pending.tool_name.clone())
    }

    pub fn pending_age_secs(&self, session_key: &str) -> Option<u64> {
        self.pending
            .get(session_key)
            .map(|pending| pending.created_at.elapsed().as_secs())
    }

    pub fn attach_long_task(
        &self,
        session_key: &str,
        file_path: String,
        done_marker: String,
        max_polls: u32,
    ) -> Result<String, AppError> {
        let task_id = format!("lt_{}", uuid_short());
        match self.pending.get_mut(session_key) {
            Some(mut pending) => {
                let lt = PendingLongTask {
                    task_id: task_id.clone(),
                    file_path,
                    done_marker,
                    polls: 0,
                    max_polls,
                    tool_call_keys: vec![session_key.to_string()],
                };
                self.long_tasks.insert(task_id.clone(), lt);
                pending.long_task_id = Some(task_id.clone());
                Ok(task_id)
            }
            None => Err(AppError::BadRequest(format!(
                "未找到等待中的工具调用会话: {}",
                session_key
            ))),
        }
    }

    pub fn record_poll_tool_use(&self, session_key: &str) -> Result<(), AppError> {
        match self.pending.get_mut(session_key) {
            Some(pending) => {
                if let Some(ref task_id) = pending.long_task_id {
                    if let Some(mut task) = self.long_tasks.get_mut(task_id) {
                        task.tool_call_keys.push(session_key.to_string());
                    }
                }
                Ok(())
            }
            None => Err(AppError::BadRequest(format!(
                "未找到等待中的工具调用会话: {}",
                session_key
            ))),
        }
    }

    pub fn link_poll_to_long_task(&self, session_key: &str, task_id: &str) -> Result<(), AppError> {
        match self.pending.get_mut(session_key) {
            Some(mut pending) => {
                pending.long_task_id = Some(task_id.to_string());
                if let Some(mut task) = self.long_tasks.get_mut(task_id) {
                    task.tool_call_keys.push(session_key.to_string());
                }
                Ok(())
            }
            None => Err(AppError::BadRequest(format!(
                "未找到等待中的工具调用会话: {}",
                session_key
            ))),
        }
    }

    pub fn long_task_for_tool_use(
        &self,
        tool_use_id: &str,
        model: &str,
    ) -> Option<PendingLongTask> {
        let session_key = format!("{}:{}", model, tool_use_id);
        let task_id = self
            .pending
            .get(&session_key)
            .and_then(|p| p.long_task_id.clone())?;
        self.long_tasks.get(&task_id).map(|t| t.clone())
    }

    pub fn increment_long_task_poll_by_task_id(&self, task_id: &str) -> Option<PendingLongTask> {
        self.long_tasks.get_mut(task_id).and_then(|mut task| {
            task.polls += 1;
            Some(task.clone())
        })
    }

    pub fn complete_long_task(&self, task_id: &str) {
        if let Some(task) = self.long_tasks.get(task_id) {
            let keys = task.tool_call_keys.clone();
            for key in &keys {
                self.pending.remove(key);
            }
        }
        self.long_tasks.remove(task_id);
    }

    #[cfg(test)]
    pub fn long_task_by_id(&self, task_id: &str) -> Option<PendingLongTask> {
        self.long_tasks.get(task_id).map(|t| t.clone())
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub fn long_task_count(&self) -> usize {
        self.long_tasks.len()
    }
}

pub fn should_use_tool_runtime(req: &MessagesRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
}

#[cfg(test)]
pub fn session_key_for_request(req: &MessagesRequest) -> Option<String> {
    latest_tool_result_id(req)
        .or_else(|| latest_tool_use_id(req))
        .map(|id| format!("{}:{}", req.model, id))
}

pub fn latest_tool_result_id(req: &MessagesRequest) -> Option<String> {
    for msg in req.messages.iter().rev() {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            for block in blocks.iter().rev() {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    return Some(tool_use_id.clone());
                }
            }
        }
    }
    None
}

#[cfg(test)]
pub fn latest_tool_use_id(req: &MessagesRequest) -> Option<String> {
    for msg in req.messages.iter().rev() {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            for block in blocks.iter().rev() {
                if let ContentBlock::ToolUse { id, .. } = block {
                    return Some(id.clone());
                }
            }
        }
    }
    None
}

#[cfg(test)]
pub fn has_matching_tool_use(req: &MessagesRequest, tool_use_id: &str) -> bool {
    req.messages.iter().any(|msg| {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == tool_use_id))
        } else {
            false
        }
    })
}

#[cfg(test)]
mod tests {
    use crate::model::anthropic::{
        AnthropicContent, AnthropicMessage, AnthropicTool, ContentBlock, MessagesRequest,
    };

    fn make_request(tools: Option<Vec<AnthropicTool>>) -> MessagesRequest {
        MessagesRequest {
            model: "claude-test".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("read a file".to_string()),
            }],
            system: None,
            max_tokens: 1024,
            stream: Some(true),
            tools,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        }
    }

    fn read_tool() -> AnthropicTool {
        AnthropicTool {
            name: "Read".to_string(),
            description: "Read file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        }
    }

    #[test]
    fn test_router_detects_tool_request() {
        assert!(!super::should_use_tool_runtime(&make_request(None)));
        assert!(!super::should_use_tool_runtime(&make_request(Some(vec![]))));
        assert!(super::should_use_tool_runtime(&make_request(Some(vec![
            read_tool()
        ]))));
    }

    #[test]
    fn test_ledger_tracks_pending_tool_result() {
        let ledger = super::ToolLedger::new();

        ledger
            .record_tool_use("session-a", "call_0", "Read")
            .unwrap();
        assert!(ledger.validate_tool_result("session-a", "call_0").is_ok());
        assert!(ledger.validate_tool_result("session-a", "call_1").is_err());
        assert_eq!(
            ledger.pending_tool_name("session-a"),
            Some("Read".to_string())
        );
        assert!(ledger.pending_age_secs("session-a").is_some());
    }

    #[test]
    fn test_session_key_uses_latest_tool_result_when_present() {
        let req = MessagesRequest {
            model: "claude-test".to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "call_0".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "Cargo.toml"}),
                    }]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "call_0".to_string(),
                        content: "contents".to_string(),
                    }]),
                },
            ],
            system: None,
            max_tokens: 1024,
            stream: Some(true),
            tools: Some(vec![read_tool()]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        };

        assert_eq!(
            super::session_key_for_request(&req),
            Some("claude-test:call_0".to_string())
        );
    }

    #[test]
    fn test_tool_result_matches_prior_tool_use_from_request_history() {
        let req = MessagesRequest {
            model: "claude-test".to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "call_7".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "Cargo.toml"}),
                    }]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "call_7".to_string(),
                        content: "contents".to_string(),
                    }]),
                },
            ],
            system: None,
            max_tokens: 1024,
            stream: Some(true),
            tools: Some(vec![read_tool()]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        };

        assert!(super::has_matching_tool_use(&req, "call_7"));
        assert!(!super::has_matching_tool_use(&req, "call_8"));
    }

    #[test]
    fn test_attach_long_task_returns_task_id_and_tracks_in_ledger() {
        let ledger = super::ToolLedger::new();
        ledger
            .record_tool_use("model-a:call_0", "call_0", "Bash")
            .unwrap();

        let task_id = ledger
            .attach_long_task(
                "model-a:call_0",
                "/tmp/task.txt".to_string(),
                "DONE".to_string(),
                8,
            )
            .unwrap();

        assert!(task_id.starts_with("lt_"));
        assert_eq!(ledger.pending_count(), 1);
        assert_eq!(ledger.long_task_count(), 1);

        let task = ledger.long_task_by_id(&task_id).unwrap();
        assert_eq!(task.task_id, task_id);
        assert_eq!(task.file_path, "/tmp/task.txt");
        assert_eq!(task.done_marker, "DONE");
        assert_eq!(task.tool_call_keys, vec!["model-a:call_0".to_string()]);

        let found = ledger.long_task_for_tool_use("call_0", "model-a").unwrap();
        assert_eq!(found.task_id, task_id);
    }

    #[test]
    fn test_complete_long_task_clears_polls_and_removes_task() {
        let ledger = super::ToolLedger::new();

        let primary_key = "model-a:call_0";
        ledger.record_tool_use(primary_key, "call_0", "Bash").unwrap();
        let task_id = ledger
            .attach_long_task(primary_key, "task.txt".to_string(), "DONE".to_string(), 8)
            .unwrap();

        let poll1 = "model-a:call_poll1";
        ledger.record_tool_use(poll1, "call_poll1", "Bash").unwrap();
        ledger.link_poll_to_long_task(poll1, &task_id).unwrap();

        let poll2 = "model-a:call_poll2";
        ledger.record_tool_use(poll2, "call_poll2", "Bash").unwrap();
        ledger.link_poll_to_long_task(poll2, &task_id).unwrap();

        assert_eq!(ledger.pending_count(), 3);
        assert_eq!(ledger.long_task_count(), 1);

        let task = ledger.long_task_by_id(&task_id).unwrap();
        assert_eq!(task.tool_call_keys.len(), 3);
        assert_eq!(task.polls, 0);

        let updated = ledger.increment_long_task_poll_by_task_id(&task_id).unwrap();
        assert_eq!(updated.polls, 1);

        ledger.complete_long_task(&task_id);

        assert_eq!(ledger.pending_count(), 0);
        assert_eq!(ledger.long_task_count(), 0);
    }
}
