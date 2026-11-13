//! 工具运行时：为不支持原生工具调用的上游模型补齐完整的工具调用闭环。
//! 子模块分工——broker 负责解析主模型输出的工具调用，context 负责统一
//! 会话时间线，protocol 负责工具规格与 ID 归一化，schema/validation 负责
//! 入参校验，payload/materialize 负责长文本传递，openai_compiler 提供外部
//! 编译器兜底，dialect 负责标记方言解析，decision/pipeline 负责决策编排与
//! 可观测元数据；本文件另提供等待工具结果的会话账本 `ToolLedger` 与
//! 请求判定辅助函数。

use std::time::Instant;

use dashmap::DashMap;

use crate::error::AppError;
use crate::model::anthropic::MessagesRequest;
#[cfg(test)]
use crate::model::anthropic::{AnthropicContent, ContentBlock};

/// 工具决策 broker：解析主模型输出并裁决调用工具还是最终回答。
pub mod broker;
/// 统一会话时间线（消息 / 工具调用 / 工具结果）的规范化表示。
pub mod context;
/// OpenAI 协议的工具决策入口与元数据。
pub mod decision;
/// 工具调用标记方言的识别与解析。
pub mod dialect;
/// 工具入参中 payload 引用的落地与长文本限制。
pub mod materialize;
/// 外部工具编译器客户端（OpenAI-compatible / Anthropic 后端）。
pub mod openai_compiler;
/// 主模型输出中长文本负载的提取。
pub mod payload;
/// 决策 + 账本 + 统计的流水线统一入口与可观测元数据。
pub mod pipeline;
/// 协议无关的工具规格、tool_choice 与 ID 归一化。
pub mod protocol;
/// 工具模型地址的自环防护与单跳标记头约定。
pub mod provider;
/// 面向编译器的工具 schema 改写（长文本字段换引用字段）。
pub mod schema;
/// 工具调用入参的 schema 校验。
pub mod validation;

#[cfg(test)]
mod broker_probe;

/// 生成短格式（无连字符）的 UUID 字符串，用作各类内部 ID 的随机成分。
pub(crate) fn uuid_short() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 账本中一次等待工具结果的待处理调用。
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    /// 等待结果的工具调用 ID。
    pub tool_use_id: String,
    /// 工具名称（当前流程未消费，保留供调试）。
    #[allow(dead_code)]
    pub tool_name: String,
    /// 该调用的登记时间，用于计算等待时长。
    pub created_at: Instant,
    /// 关联的长时任务 ID；非长时调用为 `None`。
    pub long_task_id: Option<String>,
}

/// 账本中登记的一个长时任务。
#[derive(Debug, Clone)]
pub struct PendingLongTask {
    /// 任务唯一 ID（`lt_` 前缀）。
    pub task_id: String,
    /// 完成标记所在的文件路径。
    pub file_path: String,
    /// 判定任务完成的标记文本。
    pub done_marker: String,
    /// 已执行的轮询次数。
    pub polls: u32,
    /// 允许的最大轮询次数，超过后视为失败。
    pub max_polls: u32,
    /// 与该任务关联的全部会话键（首次调用与各次轮询）。
    pub tool_call_keys: Vec<String>,
}

/// 工具会话账本：跟踪"已发出工具调用、等待结果回传"的会话与长时任务。
/// 内部为并发哈希表，线程安全。
#[derive(Default)]
pub struct ToolLedger {
    /// 等待工具结果的会话，键为会话键（模型:调用ID）。
    pending: DashMap<String, PendingToolCall>,
    /// 登记中的长时任务，键为任务 ID。
    long_tasks: DashMap<String, PendingLongTask>,
    /// 工具调用 ID 到会话键的反查索引。
    tool_use_index: DashMap<String, String>,
}

impl ToolLedger {
    /// 创建空账本。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次待处理的工具调用。
    ///
    /// 写入调用 ID 反查索引与待处理会话记录。
    ///
    /// # 参数
    /// - `session_key`：会话键（通常是 `模型:调用ID`）。
    /// - `tool_use_id`：工具调用 ID。
    /// - `tool_name`：工具名称。
    ///
    /// # 返回
    /// 登记成功返回 `Ok(())`。
    ///
    /// # 错误
    /// 任一入参为空串时返回 `AppError::BadRequest`。
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
        self.tool_use_index
            .insert(tool_use_id.to_string(), session_key.to_string());
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

    /// 校验工具结果是否与登记中的待处理调用匹配（仅供测试）。
    ///
    /// # 参数
    /// - `session_key`：会话键。
    /// - `tool_use_id`：结果回传的调用 ID。
    ///
    /// # 返回
    /// 匹配返回 `Ok(())`。
    ///
    /// # 错误
    /// 会话不存在或调用 ID 不匹配时返回 `AppError::BadRequest`。
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

    /// 校验并移除一条待处理调用，表示工具结果已回传（仅供测试）。
    ///
    /// # 参数
    /// - `session_key`：会话键。
    /// - `tool_use_id`：结果回传的调用 ID。
    ///
    /// # 返回
    /// 成功移除返回 `Ok(())`。
    ///
    /// # 错误
    /// 校验不通过（见 [`ToolLedger::validate_tool_result`]）时返回
    /// `AppError::BadRequest`。
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

    /// 查询会话当前待处理调用的工具名（仅供测试）。
    ///
    /// # 参数
    /// - `session_key`：会话键。
    ///
    /// # 返回
    /// 工具名；会话不存在时为 `None`。
    #[cfg(test)]
    pub fn pending_tool_name(&self, session_key: &str) -> Option<String> {
        self.pending
            .get(session_key)
            .map(|pending| pending.tool_name.clone())
    }

    /// 查询会话待处理调用已等待的秒数。
    ///
    /// # 参数
    /// - `session_key`：会话键。
    ///
    /// # 返回
    /// 自登记起的秒数；会话不存在时为 `None`。
    pub fn pending_age_secs(&self, session_key: &str) -> Option<u64> {
        self.pending
            .get(session_key)
            .map(|pending| pending.created_at.elapsed().as_secs())
    }

    /// 为会话的待处理调用登记一个长时任务。
    ///
    /// 创建 `lt_` 前缀的任务记录并把任务 ID 挂到该会话的待处理调用上。
    ///
    /// # 参数
    /// - `session_key`：会话键，须已通过 `record_tool_use` 登记。
    /// - `file_path`：完成标记所在文件路径。
    /// - `done_marker`：完成标记文本。
    /// - `max_polls`：最大轮询次数。
    ///
    /// # 返回
    /// 新建的任务 ID。
    ///
    /// # 错误
    /// 会话不存在时返回 `AppError::BadRequest`。
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

    /// 登记一次长时任务的轮询调用，并把它挂到会话已关联的任务上。
    ///
    /// # 参数
    /// - `session_key`：轮询调用的会话键。
    ///
    /// # 返回
    /// 登记成功返回 `Ok(())`；会话未关联任务时同样返回 `Ok`（仅登记调用）。
    ///
    /// # 错误
    /// 会话不存在时返回 `AppError::BadRequest`。
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

    /// 把轮询调用的会话显式关联到指定长时任务。
    ///
    /// 同时更新会话的任务 ID 与任务的会话键列表。
    ///
    /// # 参数
    /// - `session_key`：轮询调用的会话键。
    /// - `task_id`：目标长时任务 ID。
    ///
    /// # 返回
    /// 关联成功返回 `Ok(())`。
    ///
    /// # 错误
    /// 会话不存在时返回 `AppError::BadRequest`。
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

    /// 按工具调用 ID 反查其关联的长时任务。
    ///
    /// 先尝试 `模型:调用ID` 形式的会话键（兼容请求中模型名变化的情形），
    /// 再回退到调用 ID 反查索引；随后沿会话记录取任务。
    ///
    /// # 参数
    /// - `tool_use_id`：工具调用 ID。
    /// - `model`：当前请求的模型名，用于构造首选会话键。
    ///
    /// # 返回
    /// 关联的长时任务；找不到调用或任务时为 `None`。
    pub fn long_task_for_tool_use(
        &self,
        tool_use_id: &str,
        model: &str,
    ) -> Option<PendingLongTask> {
        let model_key = format!("{}:{}", model, tool_use_id);
        let session_key = if self.pending.contains_key(&model_key) {
            model_key
        } else {
            self.tool_use_index.get(tool_use_id)?.value().clone()
        };
        let task_id = self
            .pending
            .get(&session_key)
            .and_then(|p| p.long_task_id.clone())?;
        self.long_tasks.get(&task_id).map(|t| t.clone())
    }

    /// 将长时任务的轮询计数加一并返回更新后的任务。
    ///
    /// # 参数
    /// - `task_id`：长时任务 ID。
    ///
    /// # 返回
    /// 更新后的任务快照；任务不存在时为 `None`。
    pub fn increment_long_task_poll_by_task_id(&self, task_id: &str) -> Option<PendingLongTask> {
        self.long_tasks.get_mut(task_id).and_then(|mut task| {
            task.polls += 1;
            Some(task.clone())
        })
    }

    /// 结束一个长时任务并清理其全部关联会话。
    ///
    /// 移除任务自身以及任务记录中全部会话键对应的待处理调用与反查索引。
    ///
    /// # 参数
    /// - `task_id`：长时任务 ID；不存在时为空操作。
    pub fn complete_long_task(&self, task_id: &str) {
        if let Some(task) = self.long_tasks.get(task_id) {
            let keys = task.tool_call_keys.clone();
            for key in &keys {
                if let Some((_, pending)) = self.pending.remove(key) {
                    self.tool_use_index.remove(&pending.tool_use_id);
                }
            }
        }
        self.long_tasks.remove(task_id);
    }

    /// 按任务 ID 查询长时任务（仅供测试）。
    ///
    /// # 参数
    /// - `task_id`：长时任务 ID。
    ///
    /// # 返回
    /// 任务快照；不存在时为 `None`。
    #[cfg(test)]
    pub fn long_task_by_id(&self, task_id: &str) -> Option<PendingLongTask> {
        self.long_tasks.get(task_id).map(|t| t.clone())
    }

    /// 返回当前等待工具结果的会话数（仅供测试）。
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// 返回当前登记中的长时任务数（仅供测试）。
    #[cfg(test)]
    pub fn long_task_count(&self) -> usize {
        self.long_tasks.len()
    }
}

/// 判断请求是否需要启用工具运行时。
///
/// # 参数
/// - `req`：Anthropic Messages 请求。
///
/// # 返回
/// 请求携带非空工具定义列表时为 `true`。
pub fn should_use_tool_runtime(req: &MessagesRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
}

/// 从请求历史推导工具会话键（仅供测试）。
///
/// 优先取最新的工具结果 ID，其次取最新的工具调用 ID，
/// 组合为 `模型:ID` 形式。
///
/// # 参数
/// - `req`：Anthropic Messages 请求。
///
/// # 返回
/// 会话键；请求中无任何工具块时为 `None`。
#[cfg(test)]
pub fn session_key_for_request(req: &MessagesRequest) -> Option<String> {
    latest_tool_result_id(req)
        .or_else(|| latest_tool_use_id(req))
        .map(|id| format!("{}:{}", req.model, id))
}

/// 从请求历史中取最新的工具结果 ID（仅供测试）。
///
/// 按消息倒序、块倒序扫描，返回首个 ToolResult 块的 `tool_use_id`。
///
/// # 参数
/// - `req`：Anthropic Messages 请求。
///
/// # 返回
/// 最新工具结果 ID；不存在时为 `None`。
#[cfg(test)]
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

/// 从 OpenAI Chat 请求中提取最新的工具结果。
///
/// 按消息倒序找第一个 `role == "tool"` 的消息。
///
/// # 参数
/// - `req`：OpenAI Chat 请求。
///
/// # 返回
/// `(tool_call_id, content)` 元组；content 缺省时为空串；
/// 请求中无工具结果消息或缺少 `tool_call_id` 时为 `None`。
pub fn latest_openai_tool_result(
    req: &crate::model::openai::ChatCompletionRequest,
) -> Option<(String, String)> {
    req.messages.iter().rev().find_map(|msg| {
        if msg.role == "tool" {
            let id = msg.tool_call_id.clone()?;
            let content = msg.content.clone().unwrap_or_default();
            Some((id, content))
        } else {
            None
        }
    })
}

/// 从请求历史中取最新的工具调用 ID（仅供测试）。
///
/// 按消息倒序、块倒序扫描，返回首个 ToolUse 块的调用 ID。
///
/// # 参数
/// - `req`：Anthropic Messages 请求。
///
/// # 返回
/// 最新工具调用 ID；不存在时为 `None`。
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

/// 判断请求历史中是否存在指定 ID 的工具调用块（仅供测试）。
///
/// # 参数
/// - `req`：Anthropic Messages 请求。
/// - `tool_use_id`：目标调用 ID。
///
/// # 返回
/// 存在匹配的 ToolUse 块时为 `true`。
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
    fn test_uuid_short_uses_full_uuid_entropy() {
        let first = super::uuid_short();
        let second = super::uuid_short();
        assert_ne!(first, second);
        assert!(first.len() > 6);
        assert!(second.len() > 6);
    }

    #[test]
    fn test_normalized_duplicate_provider_ids_keep_ledger_sessions_isolated() {
        let mut ids = ["call_0".to_string(), "call_0".to_string()];
        crate::tool_runtime::protocol::normalize_tool_call_ids(ids.iter_mut());
        let ledger = super::ToolLedger::new();

        ledger
            .record_tool_use("session-a", &ids[0], "Bash")
            .unwrap();
        ledger
            .record_tool_use("session-b", &ids[1], "Bash")
            .unwrap();

        assert_ne!(ids[0], ids[1]);
        assert_eq!(ledger.pending_count(), 2);
        assert!(ledger.validate_tool_result("session-a", &ids[0]).is_ok());
        assert!(ledger.validate_tool_result("session-b", &ids[1]).is_ok());
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
                        is_error: false,
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
                        is_error: false,
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
        ledger
            .record_tool_use(primary_key, "call_0", "Bash")
            .unwrap();
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

        let updated = ledger
            .increment_long_task_poll_by_task_id(&task_id)
            .unwrap();
        assert_eq!(updated.polls, 1);

        ledger.complete_long_task(&task_id);

        assert_eq!(ledger.pending_count(), 0);
        assert_eq!(ledger.long_task_count(), 0);
    }

    #[test]
    fn test_long_task_lookup_survives_request_model_change() {
        let ledger = super::ToolLedger::new();

        ledger
            .record_tool_use("glm-5:call_0", "call_0", "Bash")
            .unwrap();
        let task_id = ledger
            .attach_long_task(
                "glm-5:call_0",
                "/tmp/task.txt".to_string(),
                "DONE".to_string(),
                8,
            )
            .unwrap();

        let found = ledger
            .long_task_for_tool_use("call_0", "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(found.task_id, task_id);

        let found_same_model = ledger.long_task_for_tool_use("call_0", "glm-5").unwrap();
        assert_eq!(found_same_model.task_id, task_id);
    }

    #[test]
    fn test_latest_openai_tool_result_id() {
        let req = crate::model::openai::ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![crate::model::openai::ChatMessage {
                role: "tool".to_string(),
                content: Some("PENDING".to_string()),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("Bash".to_string()),
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        assert_eq!(
            super::latest_openai_tool_result(&req),
            Some(("call_1".to_string(), "PENDING".to_string()))
        );
    }

    #[test]
    fn test_latest_openai_tool_result_returns_last_tool_message() {
        let req = crate::model::openai::ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![
                crate::model::openai::ChatMessage {
                    role: "tool".to_string(),
                    content: Some("first".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: Some("Bash".to_string()),
                },
                crate::model::openai::ChatMessage {
                    role: "tool".to_string(),
                    content: Some("second".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("call_2".to_string()),
                    name: Some("Bash".to_string()),
                },
            ],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        assert_eq!(
            super::latest_openai_tool_result(&req),
            Some(("call_2".to_string(), "second".to_string()))
        );
    }

    #[test]
    fn test_latest_openai_tool_result_returns_none_when_no_tool_messages() {
        let req = crate::model::openai::ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![crate::model::openai::ChatMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        assert!(super::latest_openai_tool_result(&req).is_none());
    }
}
