use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::stream::StreamExt;
use serde::Serialize;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::adapter::anthropic_request::convert_anthropic_request;
use crate::adapter::anthropic_response::{AnthropicCollector, AnthropicStreamConverter};
use crate::admin::stats::{ToolRuntimeDecision, ToolRuntimeMode as StatsToolRuntimeMode};
use crate::client::http::OuyiHttpClient;
use crate::client::session::{GuardedStream, spawn_session_cleanup};
use crate::client::sse::{SseLineBuffer, ouyi_application_error, parse_sse_line};
use crate::config::AppConfig;
use crate::error::AppError;
use crate::model::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use crate::platform::OuyiPlatform;
use crate::tool_runtime;

const TOOL_RESULT_CONTENT_MAX_BYTES: usize = 64 * 1024;

/// 工具运行时路由决策
#[derive(Debug, Clone)]
pub enum RuntimeRoute {
    OriginalOuyi,
    Compiler,
    ConfigError(String),
}

/// 模型路由信息，区分客户端模型、有效模型和欧亿模型
#[derive(Debug, Clone)]
struct ModelRouting {
    client_model: String,
    effective_model: String,
    ouyi_model: String,
}

/// 根据请求模型名和模型映射器解析路由信息
fn resolve_model_routing(
    req_model: &str,
    mapper: &crate::model::mapper::ModelMapper,
) -> ModelRouting {
    let client_model = req_model.to_string();
    let effective_model = req_model.to_string();
    let ouyi_model = mapper.to_ouyi(&effective_model);
    ModelRouting {
        client_model,
        effective_model,
        ouyi_model,
    }
}

/// 根据请求与配置返回工具运行时路由决策
pub fn runtime_route(
    req: &MessagesRequest,
    tool_runtime_config: Option<&crate::config::ToolRuntimeConfig>,
) -> RuntimeRoute {
    // tools 为空 → 原 Ouyi 链路
    if !tool_runtime::should_use_tool_runtime(req) {
        return RuntimeRoute::OriginalOuyi;
    }

    // tools 非空，且未配置 tool_runtime → 走 Compiler 路由，broker 负责解析
    // broker 会在标记/规则解析都失败且无 config 时返回 ConfigError
    if tool_runtime_config.is_none() {
        return RuntimeRoute::Compiler;
    }
    let config = tool_runtime_config.unwrap();

    let provider_supported = matches!(
        config.provider,
        crate::config::ToolRuntimeProvider::Anthropic
            | crate::config::ToolRuntimeProvider::OpenaiCompatible
    );
    if provider_supported {
        RuntimeRoute::Compiler
    } else {
        RuntimeRoute::ConfigError("工具模型接口类型不支持".to_string())
    }
}

#[derive(Serialize)]
struct AnthropicModelEntry {
    id: String,
    #[serde(rename = "type")]
    ty: &'static str,
    display_name: String,
}

#[derive(Serialize)]
struct AnthropicModelList {
    data: Vec<AnthropicModelEntry>,
}

pub async fn list_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let short_names = state.model_mapper.get_all_short_names();
    let models: Vec<AnthropicModelEntry> = short_names
        .into_iter()
        .map(|id| AnthropicModelEntry {
            display_name: id.clone(),
            id,
            ty: "model",
        })
        .collect();
    Json(AnthropicModelList { data: models })
}

/// 从请求中提取最后一个 tool_use 的 id，用作会话缓存 key。
/// Claude Code 回传 tool_result 时，消息中一定包含对应的 tool_use_id。
fn extract_last_tool_use_id(req: &MessagesRequest) -> Option<String> {
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

/// 检测请求是否包含 tool_result 内容块
fn has_tool_result(req: &MessagesRequest) -> bool {
    req.messages.iter().any(|msg| {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        } else {
            false
        }
    })
}

fn tool_count(req: &MessagesRequest) -> usize {
    req.tools.as_ref().map(|tools| tools.len()).unwrap_or(0)
}

/// 生成全局工具会话缓存 key（模型无关）
fn global_tool_session_cache_key(tool_use_id: &str) -> String {
    format!("tool:{}", tool_use_id)
}

/// 生成会话缓存 key：模型名 + 最后一个 tool_use_id
fn session_cache_key(model: &str, tool_use_id: &str) -> String {
    format!("{}:{}", model, tool_use_id)
}

fn session_cache_keys(primary_model: &str, request_model: &str, tool_use_id: &str) -> Vec<String> {
    let mut keys = vec![global_tool_session_cache_key(tool_use_id)];
    for model in [primary_model, request_model] {
        let key = session_cache_key(model, tool_use_id);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

fn take_cached_tool_session(
    state: &AppState,
    primary_model: &str,
    request_model: &str,
    tool_use_id: &str,
) -> Option<(String, crate::CachedToolSession)> {
    let keys = session_cache_keys(primary_model, request_model, tool_use_id);
    let mut found = None;
    for key in &keys {
        if let Some((removed_key, cached)) = state.tool_sessions.remove(key)
            && found.is_none()
        {
            found = Some((removed_key, cached));
        }
    }
    // 清理所有相关 keys（跨模型复用时可能有多个残留）
    if found.is_some() {
        for key in &keys {
            state.tool_sessions.remove(key);
        }
    }
    found
}

fn insert_cached_tool_session(
    state: &AppState,
    _primary_model: &str,
    _request_model: &str,
    tool_use_id: &str,
    session_id: i64,
) {
    // 只存储全局 key，实现跨模型复用
    let key = global_tool_session_cache_key(tool_use_id);
    state.tool_sessions.insert(
        key,
        crate::CachedToolSession {
            session_id,
            created_at: std::time::Instant::now(),
        },
    );
}

fn truncate_utf8_by_byte_limit_with_notice(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let char_count = content.chars().count();
    let truncated: String = content
        .chars()
        .scan(0usize, |bytes, ch| {
            let next = *bytes + ch.len_utf8();
            if next > max_bytes {
                None
            } else {
                *bytes = next;
                Some(ch)
            }
        })
        .collect();
    format!("{truncated}\n\n[...内容截断，共 {char_count} 字符...]")
}

/// 从请求中提取 tool_result 内容，格式化为下游客话增量文本。
/// 仅用于会话复用时发送 tool_result，不包含系统提示和工具指令。
fn format_tool_results_for_reuse(req: &MessagesRequest) -> String {
    let tool_id_to_name = build_tool_id_to_name(req);
    let mut parts = Vec::new();
    for msg in &req.messages {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } = block
                {
                    let tool_name = tool_id_to_name
                        .get(tool_use_id.as_str())
                        .map(|s| s.as_str())
                        .unwrap_or(tool_use_id.as_str());
                    let display_content = truncate_utf8_by_byte_limit_with_notice(
                        content,
                        TOOL_RESULT_CONTENT_MAX_BYTES,
                    );
                    parts.push(format!(
                        "工具 {} 的执行结果：\n{}",
                        tool_name, display_content
                    ));
                }
            }
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    parts.push(
        "请根据以上工具执行结果继续。如果需要再次调用工具，请表达工具意图；长文本继续使用 ouyi_payload。".to_string(),
    );
    parts.join("\n\n")
}

fn merge_ouyi_text_parts(parts: &[String]) -> String {
    let mut output = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if part == &output {
            continue;
        }
        if part.starts_with(&output) {
            output = part.clone();
        } else {
            output.push_str(part);
        }
    }
    output
}

fn latest_user_plain_text(req: &MessagesRequest) -> Option<String> {
    req.messages
        .iter()
        .rev()
        .find(|msg| msg.role == "user")
        .and_then(|msg| match &msg.content {
            AnthropicContent::Text(text) => Some(text.trim().to_string()),
            AnthropicContent::Blocks(blocks) => blocks.iter().rev().find_map(|block| {
                if let ContentBlock::Text { text } = block {
                    Some(text.trim().to_string())
                } else {
                    None
                }
            }),
        })
        .filter(|text| !text.is_empty())
}

fn latest_tool_result_content(req: &MessagesRequest) -> Option<(String, String)> {
    req.messages.iter().rev().find_map(|msg| {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            blocks.iter().rev().find_map(|block| {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } = block
                {
                    Some((tool_use_id.clone(), content.clone()))
                } else {
                    None
                }
            })
        } else {
            None
        }
    })
}

fn maybe_build_long_task_poll_response(
    state: &AppState,
    req: &MessagesRequest,
    model: &str,
) -> Result<Option<axum::response::Response>, AppError> {
    let Some((tool_use_id, content)) = latest_tool_result_content(req) else {
        return Ok(None);
    };
    let Some(task) = state
        .tool_ledger
        .long_task_for_tool_use(&tool_use_id, &req.model)
    else {
        return Ok(None);
    };

    use crate::tool_runtime::broker::LongTaskResult;
    match crate::tool_runtime::broker::classify_long_task_result(&content, &task.done_marker) {
        LongTaskResult::Done => {
            // 长时任务完成，清理 ledger 中的 pending 条目和 long task
            state.tool_ledger.complete_long_task(&task.task_id);
            Ok(None)
        }
        LongTaskResult::Failed(msg) => Err(AppError::Upstream(msg)),
        LongTaskResult::Pending => {
            let updated = state
                .tool_ledger
                .increment_long_task_poll_by_task_id(&task.task_id)
                .ok_or_else(|| AppError::BadRequest("长时任务状态不存在".to_string()))?;
            if updated.polls > updated.max_polls {
                state.tool_ledger.complete_long_task(&task.task_id);
                return Err(AppError::Upstream(format!(
                    "长时任务轮询超限: {}",
                    updated.file_path
                )));
            }
            let plan = crate::tool_runtime::broker::LongTaskPlan {
                file_path: updated.file_path,
                done_marker: updated.done_marker,
                interval_secs: 5,
                max_polls: updated.max_polls,
            };
            let call = crate::tool_runtime::broker::build_poll_call(
                &plan,
                &req.tools
                    .as_ref()
                    .map(|tools| {
                        crate::tool_runtime::protocol::anthropic_tools_to_specs(tools.as_slice())
                    })
                    .unwrap_or_default(),
            )
            .map_err(AppError::BadRequest)?;
            let poll_key = format!("{}:{}", req.model, call.id);
            state
                .tool_ledger
                .record_tool_use(&poll_key, &call.id, "Bash")?;
            state
                .tool_ledger
                .link_poll_to_long_task(&poll_key, &task.task_id)?;

            let calls = vec![crate::tool_runtime::openai_compiler::CompiledToolCall {
                id: call.id,
                name: call.name,
                input: call.input,
            }];
            build_tool_use_response(model, &format!("msg_{}", uuid_short()), calls, 0, 0).map(Some)
        }
    }
}

fn split_plain_text_blocks(text: &str, latest_user_text: Option<&str>) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut blocks = Vec::new();
    let mut current = Vec::new();

    for line in normalized.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(current.join("\n").trim().to_string());
                current.clear();
            }
        } else {
            current.push(line.trim_end().to_string());
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n").trim().to_string());
    }

    if blocks.len() > 1 {
        return blocks;
    }

    let lines: Vec<String> = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let has_line_snapshot_signal = lines.windows(2).any(|pair| {
        pair[0] == pair[1] || (pair[1].starts_with(&pair[0]) && pair[0].chars().count() <= 20)
    });
    let has_user_echo_signal = latest_user_text
        .filter(|latest| lines.len() > 1 && lines.first().is_some_and(|line| line == latest))
        .is_some();
    if has_line_snapshot_signal || has_user_echo_signal {
        lines
    } else {
        blocks
    }
}

fn normalize_plain_text(text: &str, latest_user_text: Option<&str>) -> String {
    let merged = merge_ouyi_text_parts(&[text.to_string()]);
    let trimmed = merged.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let blocks = split_plain_text_blocks(trimmed, latest_user_text);
    if blocks.len() <= 1 {
        return trimmed.to_string();
    }

    let mut normalized = Vec::new();
    for (idx, block) in blocks.iter().enumerate() {
        if normalized.last().is_some_and(|last| last == block) {
            continue;
        }
        let is_user_echo = latest_user_text.is_some_and(|latest| latest == block)
            && blocks
                .get(idx + 1)
                .is_some_and(|next| next.starts_with(block) || next != block);
        if is_user_echo {
            continue;
        }
        let is_short_prefix_snapshot = blocks
            .get(idx + 1)
            .is_some_and(|next| next.starts_with(block) && block.chars().count() <= 20);
        if is_short_prefix_snapshot {
            continue;
        }
        normalized.push(block.as_str());
    }

    normalized.join("\n\n")
}

fn normalize_ouyi_display_text(text: &str, latest_user_text: Option<&str>) -> String {
    let mut tool_parser = crate::adapter::tool_call::ToolCallParser::new();
    let parsed = tool_parser.process_chunk(text);
    normalize_plain_text(&parsed.normal_text, latest_user_text)
}

async fn request_final_answer_after_suppressed_tool_intent(
    client: &OuyiHttpClient,
    session_id: i64,
    thinking: bool,
) -> Result<(String, u64, u64), AppError> {
    let prompt = concat!(
        "上一次输出的工具调用已由系统处理，不需要再次输出工具调用标记。\n",
        "请基于当前会话上下文和已有工具结果，直接给出面向用户的最终回答。"
    )
    .to_string();
    let resp = client.chat_stream(session_id, prompt, thinking).await?;
    collect_ouyi_output(resp).await
}

fn build_tool_id_to_name(req: &MessagesRequest) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for msg in &req.messages {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    map.insert(id.clone(), name.clone());
                }
            }
        }
    }
    map
}

/// 清理过期工具会话缓存（超过 5 分钟）
fn prune_stale_tool_sessions(state: &AppState, config: &AppConfig) {
    let mut to_remove = Vec::new();
    for entry in state.tool_sessions.iter() {
        if entry.value().created_at.elapsed().as_secs() > 300 {
            to_remove.push(entry.key().clone());
        }
    }
    for key in to_remove {
        if let Some((_, cached)) = state.tool_sessions.remove(&key) {
            tracing::debug!(session_id = cached.session_id, "清理过期工具会话缓存");
            let jwt_token = config.ouyi.jwt_token.clone();
            if !jwt_token.is_empty() {
                let client = OuyiHttpClient::new(
                    state.http_client.clone(),
                    config.ouyi.base_url.clone(),
                    jwt_token,
                    config.ouyi.app_version.clone(),
                );
                spawn_session_cleanup(client, cached.session_id);
            }
        }
    }
}

pub async fn messages(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MessagesRequest>,
) -> Result<axum::response::Response, AppError> {
    let routing = resolve_model_routing(&req.model, &state.model_mapper);
    let config = state.config.read().await.clone();
    let route = runtime_route(&req, config.tool_runtime.as_ref());
    let route_name = match &route {
        RuntimeRoute::OriginalOuyi => "ouyi",
        RuntimeRoute::Compiler => "compiler",
        RuntimeRoute::ConfigError(_) => "config_error",
    };
    let tools_count = tool_count(&req);
    tracing::info!(
        route = route_name,
        ouyi_model = %routing.ouyi_model,
        ouyi_base_url = %config.ouyi.base_url,
        messages_count = req.messages.len(),
        tools = tools_count,
        "主模型请求"
    );
    tracing::debug!(
        route = route_name,
        client_model = %routing.client_model,
        effective_model = %routing.effective_model,
        ouyi_model = %routing.ouyi_model,
        messages_count = req.messages.len(),
        stream = ?req.stream,
        tools = tools_count,
        "模型路由详情"
    );

    // 优先检查长时任务轮询（broker continuation 统一入口）
    if has_tool_result(&req) {
        if let Some(response) = maybe_build_long_task_poll_response(&state, &req, &req.model)? {
            return Ok(response);
        }
    }

    // 按平台分派
    let platform = *state.platform.read().await;
    match platform {
        OuyiPlatform::Ouyi7 => handle_ouyi7_message(state, req, routing).await,
        OuyiPlatform::Ouyi8 => match route {
            RuntimeRoute::OriginalOuyi => handle_original_ouyi_message(state, req, routing).await,
            RuntimeRoute::Compiler => {
                handle_compiler_tool_runtime_message(state, req, routing).await
            }
            RuntimeRoute::ConfigError(msg) => Err(AppError::Config(msg)),
        },
    }
}

fn record_anthropic_error(
    state: &AppState,
    start_time: std::time::Instant,
    model: &str,
    error: &AppError,
    platform: &str,
) {
    state.stats.record_error(
        start_time.elapsed().as_millis() as u64,
        model,
        &error.to_string(),
        platform,
    );
}

/// 原 Ouyi 链路处理（无工具或未配置工具运行时）
async fn handle_original_ouyi_message(
    state: Arc<AppState>,
    req: MessagesRequest,
    routing: ModelRouting,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    tracing::debug!(
        client_model = %routing.client_model,
        effective_model = %routing.effective_model,
        ouyi_model = %routing.ouyi_model,
        "模型映射结果"
    );

    let short_model = routing.client_model.clone();
    let model_full = routing.ouyi_model.clone();
    let should_thinking = state.model_mapper.should_thinking(&routing.effective_model)
        || matches!(
            req.thinking,
            Some(crate::model::anthropic::ThinkingConfig::Enabled { .. })
                | Some(crate::model::anthropic::ThinkingConfig::Adaptive)
        );

    prune_stale_tool_sessions(&state, &config);

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    let ouyi_req = match convert_anthropic_request(&req, &model_full, should_thinking) {
        Ok(req) => req,
        Err(e) => {
            let err = AppError::BadRequest(e);
            record_anthropic_error(&state, start_time, &model_full, &err, "ouyi8");
            return Err(err);
        }
    };

    // 判断是否为工具回传请求，尝试复用缓存会话
    let tool_use_id = extract_last_tool_use_id(&req);
    let is_tool_continuation = has_tool_result(&req) && tool_use_id.is_some();

    let (session_id, client, cache_key) = if is_tool_continuation {
        let tid = tool_use_id.as_ref().unwrap();
        match take_cached_tool_session(&state, &model_full, &short_model, tid) {
            Some((key, cached)) => {
                tracing::debug!(session_id = cached.session_id, tool_use_id = %tid, "复用工具会话");
                // 复用缓存的 session_id，但创建新的 client
                let jwt_token = match state.account_store.get_next_token().await {
                    Some(token) => token,
                    None => config.ouyi.jwt_token.clone(),
                };
                if jwt_token.is_empty() {
                    let err =
                        AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string());
                    record_anthropic_error(&state, start_time, &model_full, &err, "ouyi8");
                    return Err(err);
                }
                let client = OuyiHttpClient::new(
                    state.http_client.clone(),
                    config.ouyi.base_url.clone(),
                    jwt_token,
                    config.ouyi.app_version.clone(),
                );
                (cached.session_id, client, Some(key))
            }
            None => {
                tracing::debug!(tool_use_id = %tid, "工具会话缓存未命中，创建新会话");
                let jwt_token = match state.account_store.get_next_token().await {
                    Some(token) => token,
                    None => config.ouyi.jwt_token.clone(),
                };
                if jwt_token.is_empty() {
                    let err =
                        AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string());
                    record_anthropic_error(&state, start_time, &model_full, &err, "ouyi8");
                    return Err(err);
                }
                let client = OuyiHttpClient::new(
                    state.http_client.clone(),
                    config.ouyi.base_url.clone(),
                    jwt_token,
                    config.ouyi.app_version.clone(),
                );
                let session_data = match client.create_session(&model_full).await {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = format!("创建会话失败: {}", e);
                        state.stats.record_error(
                            start_time.elapsed().as_millis() as u64,
                            &model_full,
                            &msg,
                            "ouyi8",
                        );
                        return Err(AppError::Upstream(msg));
                    }
                };
                (session_data.id, client, None)
            }
        }
    } else {
        let jwt_token = match state.account_store.get_next_token().await {
            Some(token) => token,
            None => config.ouyi.jwt_token.clone(),
        };
        if jwt_token.is_empty() {
            let err = AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string());
            record_anthropic_error(&state, start_time, &model_full, &err, "ouyi8");
            return Err(err);
        }
        let client = OuyiHttpClient::new(
            state.http_client.clone(),
            config.ouyi.base_url.clone(),
            jwt_token,
            config.ouyi.app_version.clone(),
        );
        let session_data = match client.create_session(&model_full).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("创建会话失败: {}", e);
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    &model_full,
                    &msg,
                    "ouyi8",
                );
                return Err(AppError::Upstream(msg));
            }
        };
        (session_data.id, client, None)
    };

    state.active_sessions.insert(session_id, ());

    let final_text = if is_tool_continuation && cache_key.is_some() {
        // Cache HIT: session 已包含完整上下文（工具指令 + 历史消息）
        // 仅发送 tool_result 增量，避免重复系统提示导致模型误入工具循环
        format_tool_results_for_reuse(&req)
    } else if let Some(sys_prompt) = &ouyi_req.system_prompt {
        format!("[System]: {}\n\n{}", sys_prompt, ouyi_req.text)
    } else {
        ouyi_req.text.clone()
    };

    let resp = match client
        .chat_stream(session_id, final_text, ouyi_req.thinking)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            state.active_sessions.remove(&session_id);
            let msg = format!("聊天请求失败: {}", e);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                "ouyi8",
            );
            spawn_session_cleanup(client, session_id);
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;
    let stream = req.stream.unwrap_or(false);

    if stream {
        handle_anthropic_stream(
            resp,
            short_model,
            session_id,
            client,
            state.clone(),
            cache_key,
            start_time,
        )
        .await
    } else {
        handle_anthropic_non_stream(
            resp,
            short_model,
            session_id,
            client,
            state.clone(),
            cache_key,
            elapsed,
        )
        .await
    }
}

/// 7.0 平台消息处理
async fn handle_ouyi7_message(
    state: Arc<AppState>,
    req: MessagesRequest,
    routing: ModelRouting,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();

    let v7_config = config
        .ouyi_v7
        .as_ref()
        .ok_or_else(|| AppError::Config("未配置 7.0 平台".to_string()))?;

    let model_full = routing.ouyi_model.clone();
    let short_model = routing.client_model.clone();
    let max_history = v7_config.max_history_messages;

    let has_tools = req.tools.as_ref().is_some_and(|tools| !tools.is_empty());

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    // 请求转换
    let ouyi_input = match crate::adapter::ouyi_v7_request::from_anthropic_messages(
        &req,
        model_full.clone(),
        max_history,
    ) {
        Ok(input) => input,
        Err(e) => {
            record_anthropic_error(
                &state,
                start_time,
                &model_full,
                &e,
                state.platform.read().await.config_key(),
            );
            return Err(e);
        }
    };

    let client = {
        let ouyi_client = state.ouyi_client.read().await;
        match &*ouyi_client {
            crate::client::OuyiClient::V7(client) => client.clone(),
            crate::client::OuyiClient::V8(_) => {
                let err = AppError::Internal("当前运行客户端不是 7.0".to_string());
                record_anthropic_error(
                    &state,
                    start_time,
                    &model_full,
                    &err,
                    state.platform.read().await.config_key(),
                );
                return Err(err);
            }
        }
    };

    let text = match client.chat_once(&ouyi_input).await {
        Ok(text) => text,
        Err(e) => {
            let msg = format!("7.0 聊天请求失败: {}", e);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                state.platform.read().await.config_key(),
            );
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;
    let platform_key = state.platform.read().await.config_key();

    if has_tools {
        handle_anthropic_tools_plain_output(
            state,
            req,
            routing,
            text,
            0,
            0,
            start_time,
            platform_key,
            None,
        )
        .await
    } else if req.stream.unwrap_or(false) {
        handle_v7_anthropic_stream(&text, &short_model, model_full, state, start_time).await
    } else {
        handle_v7_anthropic_non_stream(
            &text,
            &short_model,
            model_full,
            state,
            elapsed,
            &platform_key,
        )
        .await
    }
}

async fn handle_v7_anthropic_stream(
    text: &str,
    model: &str,
    model_full: String,
    state: Arc<AppState>,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    use crate::adapter::ouyi_v7_response::build_anthropic_stream_from_plain;

    let config = state.config.read().await;
    let chunk_chars = config
        .ouyi_v7
        .as_ref()
        .map(|v| v.simulated_stream_chunk_chars)
        .unwrap_or(80);

    let task_id = format!("msg_{}", uuid_short());
    let response = match build_anthropic_stream_from_plain(model, &task_id, text, chunk_chars) {
        Ok(response) => response,
        Err(e) => {
            let platform_key = state.platform.read().await.config_key();
            record_anthropic_error(&state, start_time, &model_full, &e, &platform_key);
            return Err(e);
        }
    };

    let stream_elapsed = start_time.elapsed().as_millis() as u64;
    let platform_key = state.platform.read().await.config_key();
    state
        .stats
        .record_success(stream_elapsed, 0, 0, &model_full, &platform_key);

    Ok(response)
}

async fn handle_v7_anthropic_non_stream(
    text: &str,
    model: &str,
    model_full: String,
    state: Arc<AppState>,
    elapsed: u64,
    platform_key: &str,
) -> Result<axum::response::Response, AppError> {
    use crate::adapter::ouyi_v7_response::build_anthropic_message_from_plain;

    let task_id = format!("msg_{}", uuid_short());
    let response = build_anthropic_message_from_plain(model, &task_id, text);

    state
        .stats
        .record_success(elapsed, 0, 0, &model_full, platform_key);

    let mut resp = Json(response).into_response();
    resp.headers_mut()
        .insert("anthropic-version", "2023-06-01".parse().unwrap());
    resp.headers_mut()
        .insert("x-request-id", task_id.parse().unwrap());
    Ok(resp)
}

/// 工具模型编译器处理（Ouyi 主脑 + 第三方工具模型）
async fn handle_compiler_tool_runtime_message(
    state: Arc<AppState>,
    req: MessagesRequest,
    routing: ModelRouting,
) -> Result<axum::response::Response, AppError> {
    use crate::tool_runtime::broker::BrokerDecision;

    let app_config = state.config.read().await.clone();
    let tool_runtime_config = app_config.tool_runtime.as_ref();

    let short_model = routing.client_model.clone();
    let model_full = routing.ouyi_model.clone();
    let should_thinking = state.model_mapper.should_thinking(&routing.effective_model)
        || matches!(
            req.thinking,
            Some(crate::model::anthropic::ThinkingConfig::Enabled { .. })
        );

    let ouyi_req = convert_anthropic_request(&req, &model_full, should_thinking)
        .map_err(|e| AppError::BadRequest(e))?;

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    let tool_use_id = extract_last_tool_use_id(&req);
    let is_tool_continuation = has_tool_result(&req) && tool_use_id.is_some();

    let jwt_token = match state.account_store.get_next_token().await {
        Some(token) => token,
        None => app_config.ouyi.jwt_token.clone(),
    };
    if jwt_token.is_empty() {
        let err = AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string());
        record_anthropic_error(&state, start_time, &model_full, &err, "ouyi8");
        return Err(err);
    }

    let client = OuyiHttpClient::new(
        state.http_client.clone(),
        app_config.ouyi.base_url.clone(),
        jwt_token,
        app_config.ouyi.app_version.clone(),
    );

    let (session_id, cache_key) = if is_tool_continuation {
        let tid = tool_use_id.as_ref().unwrap();
        match take_cached_tool_session(&state, &model_full, &short_model, tid) {
            Some((key, cached)) => {
                tracing::debug!(
                    session_id = cached.session_id,
                    tool_use_id = %tid,
                    "Compiler 模式复用工具会话"
                );
                (cached.session_id, Some(key))
            }
            None => {
                tracing::debug!(tool_use_id = %tid, "Compiler 工具会话缓存未命中，创建新会话");
                let session_data = match client.create_session(&model_full).await {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = format!("创建会话失败: {}", e);
                        state.stats.record_error(
                            start_time.elapsed().as_millis() as u64,
                            &model_full,
                            &msg,
                            "ouyi8",
                        );
                        return Err(AppError::Upstream(msg));
                    }
                };
                (session_data.id, None)
            }
        }
    } else {
        let session_data = match client.create_session(&model_full).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("创建会话失败: {}", e);
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    &model_full,
                    &msg,
                    "ouyi8",
                );
                return Err(AppError::Upstream(msg));
            }
        };
        (session_data.id, None)
    };
    state.active_sessions.insert(session_id, ());

    // 发送请求到 Ouyi
    let final_text = if is_tool_continuation && cache_key.is_some() {
        format_tool_results_for_reuse(&req)
    } else if let Some(sys_prompt) = &ouyi_req.system_prompt {
        format!("[System]: {}\n\n{}", sys_prompt, ouyi_req.text)
    } else {
        ouyi_req.text.clone()
    };

    let resp = match client
        .chat_stream(session_id, final_text, ouyi_req.thinking)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            state.active_sessions.remove(&session_id);
            let msg = format!("聊天请求失败: {}", e);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                "ouyi8",
            );
            spawn_session_cleanup(client, session_id);
            return Err(AppError::Upstream(msg));
        }
    };

    // 收集完整 Ouyi 输出
    let (ouyi_output, prompt_tokens, completion_tokens) = match collect_ouyi_output(resp).await {
        Ok(output) => output,
        Err(e) => {
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &e.to_string(),
                "ouyi8",
            );
            state.active_sessions.remove(&session_id);
            spawn_session_cleanup(client, session_id);
            return Err(e);
        }
    };

    tracing::debug!(
        ouyi_output_len = ouyi_output.len(),
        prompt_tokens,
        completion_tokens,
        "Ouyi 主脑输出完成"
    );

    let broker_started = std::time::Instant::now();
    let (broker_mode, broker_model, broker_provider, broker_base_url) = match tool_runtime_config {
        Some(cfg) => {
            let mode = StatsToolRuntimeMode::Broker;
            let model = cfg.model.clone().unwrap_or_else(|| "unknown".to_string());
            let base_url = cfg.base_url.clone().unwrap_or_default();
            let provider = match cfg.provider {
                crate::config::ToolRuntimeProvider::Anthropic => "anthropic",
                crate::config::ToolRuntimeProvider::OpenaiCompatible => "openai-compatible",
            };
            (mode, model, provider, base_url)
        }
        None => {
            let mode = StatsToolRuntimeMode::Broker;
            let model = "rule-based".to_string();
            let provider = "rule-based";
            let base_url = String::new();
            (mode, model, provider, base_url)
        }
    };

    tracing::info!(
        provider = %broker_provider,
        model = %broker_model,
        base_url = %broker_base_url,
        "工具模型请求"
    );
    let broker_result = match tool_runtime::broker::decide_with_broker(
        state.http_client.clone(),
        tool_runtime_config,
        &req,
        &ouyi_output,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            state.stats.record_tool_runtime_error(
                &format!("msg_{}", uuid_short()),
                "anthropic",
                broker_mode.clone(),
                broker_provider,
                &broker_model,
                &broker_base_url,
                ToolRuntimeDecision::FinalAnswer,
                Vec::new(),
                0,
                0,
                broker_started.elapsed().as_millis() as u64,
                &e.to_string(),
            );
            tracing::warn!(error = %e, "工具决策异常，返回主脑文本");
            BrokerDecision::FinalAnswer { usage: None }
        }
    };

    let task_id = format!("msg_{}", uuid_short());

    match broker_result {
        BrokerDecision::ToolCalls(calls) => {
            tracing::info!(
                tool_count = calls.len(),
                tools = calls
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                "工具决策"
            );

            let compiled_calls = calls
                .iter()
                .map(|c| crate::tool_runtime::openai_compiler::CompiledToolCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    input: c.input.clone(),
                })
                .collect::<Vec<_>>();

            // 缓存会话用于后续 tool_result
            for call in &compiled_calls {
                insert_cached_tool_session(&state, &model_full, &short_model, &call.id, session_id);
            }

            // 记录工具调用到 ledger
            for call in &compiled_calls {
                let key = format!("{}:{}", req.model, call.id);
                if let Err(e) = state
                    .tool_ledger
                    .record_tool_use(&key, &call.id, &call.name)
                {
                    record_anthropic_error(&state, start_time, &model_full, &e, "ouyi8");
                    return Err(e);
                }

                // 检测长时任务并附加状态
                let broker_call = crate::tool_runtime::broker::BrokerToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                };
                if let Some(plan) = crate::tool_runtime::broker::detect_long_task(&broker_call) {
                    if let Err(e) = state.tool_ledger.attach_long_task(
                        &key,
                        plan.file_path,
                        plan.done_marker,
                        plan.max_polls,
                    ) {
                        record_anthropic_error(&state, start_time, &model_full, &e, "ouyi8");
                        return Err(e);
                    }
                }
            }

            state.stats.record_success(
                start_time.elapsed().as_millis() as u64,
                prompt_tokens,
                completion_tokens,
                &short_model,
                "ouyi8",
            );
            state.stats.record_tool_runtime_success(
                &task_id,
                "anthropic",
                broker_mode.clone(),
                broker_provider,
                &broker_model,
                &broker_base_url,
                ToolRuntimeDecision::ToolCalls,
                compiled_calls.iter().map(|c| c.name.clone()).collect(),
                0,
                0,
                broker_started.elapsed().as_millis() as u64,
            );

            // 合成 Anthropic tool_use 响应
            let stream = req.stream.unwrap_or(false);
            if stream {
                build_tool_use_stream_response(
                    &short_model,
                    &task_id,
                    compiled_calls,
                    prompt_tokens,
                    completion_tokens,
                )
            } else {
                build_tool_use_response(
                    &short_model,
                    &task_id,
                    compiled_calls,
                    prompt_tokens,
                    completion_tokens,
                )
            }
        }
        BrokerDecision::FinalAnswer { usage } => {
            tracing::info!("工具决策：最终回答");
            let latest_user_text = latest_user_plain_text(&req);
            let (response_text, final_prompt_tokens, final_completion_tokens) =
                match normalize_ouyi_display_text(&ouyi_output, latest_user_text.as_deref()) {
                    text if !text.is_empty() => (text, prompt_tokens, completion_tokens),
                    _ => {
                        tracing::debug!("主脑展示文本为空，请求 Ouyi 补全最终回答");
                        let (final_output, answer_prompt_tokens, answer_completion_tokens) =
                            request_final_answer_after_suppressed_tool_intent(
                                &client,
                                session_id,
                                ouyi_req.thinking,
                            )
                            .await
                            .map_err(|e| {
                                state.stats.record_error(
                                    start_time.elapsed().as_millis() as u64,
                                    &model_full,
                                    &e.to_string(),
                                    "ouyi8",
                                );
                                state.active_sessions.remove(&session_id);
                                spawn_session_cleanup(client.clone(), session_id);
                                e
                            })?;
                        (
                            normalize_plain_text(&final_output, latest_user_text.as_deref()),
                            prompt_tokens + answer_prompt_tokens,
                            completion_tokens + answer_completion_tokens,
                        )
                    }
                };

            state.stats.record_success(
                start_time.elapsed().as_millis() as u64,
                final_prompt_tokens,
                final_completion_tokens,
                &short_model,
                "ouyi8",
            );
            let (compiler_prompt, compiler_completion) = match &usage {
                Some(u) => (u.prompt_tokens, u.completion_tokens),
                None => (0, 0),
            };
            state.stats.record_tool_runtime_success(
                &task_id,
                "anthropic",
                broker_mode.clone(),
                broker_provider,
                &broker_model,
                &broker_base_url,
                ToolRuntimeDecision::FinalAnswer,
                Vec::new(),
                compiler_prompt,
                compiler_completion,
                broker_started.elapsed().as_millis() as u64,
            );

            // 清理会话
            state.active_sessions.remove(&session_id);
            spawn_session_cleanup(client, session_id);

            // 合成 Anthropic text 响应
            let stream = req.stream.unwrap_or(false);
            if stream {
                build_text_stream_response(
                    &short_model,
                    &task_id,
                    &response_text,
                    final_prompt_tokens,
                    final_completion_tokens,
                )
            } else {
                build_text_response(
                    &short_model,
                    &task_id,
                    &response_text,
                    final_prompt_tokens,
                    final_completion_tokens,
                )
            }
        }
    }
}

/// 处理纯文本输出后的 Anthropic 工具决策响应
async fn handle_anthropic_tools_plain_output(
    state: Arc<AppState>,
    req: MessagesRequest,
    routing: ModelRouting,
    ouyi_output: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    start_time: std::time::Instant,
    platform_key: &'static str,
    reusable_session: Option<(OuyiHttpClient, i64)>,
) -> Result<axum::response::Response, AppError> {
    use crate::tool_runtime::broker::BrokerDecision;

    let app_config = state.config.read().await.clone();
    let tool_runtime_config = app_config.tool_runtime.as_ref();

    let short_model = routing.client_model.clone();
    let model_full = routing.ouyi_model.clone();

    let broker_started = std::time::Instant::now();
    let (broker_mode, broker_model, broker_provider, broker_base_url) = match tool_runtime_config {
        Some(cfg) => {
            let mode = StatsToolRuntimeMode::Broker;
            let model = cfg.model.clone().unwrap_or_else(|| "unknown".to_string());
            let base_url = cfg.base_url.clone().unwrap_or_default();
            let provider = match cfg.provider {
                crate::config::ToolRuntimeProvider::Anthropic => "anthropic",
                crate::config::ToolRuntimeProvider::OpenaiCompatible => "openai-compatible",
            };
            (mode, model, provider, base_url)
        }
        None => {
            let mode = StatsToolRuntimeMode::Broker;
            let model = "rule-based".to_string();
            let provider = "rule-based";
            let base_url = String::new();
            (mode, model, provider, base_url)
        }
    };

    let broker_result = match tool_runtime::broker::decide_with_broker(
        state.http_client.clone(),
        tool_runtime_config,
        &req,
        &ouyi_output,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            state.stats.record_tool_runtime_error(
                &format!("msg_{}", uuid_short()),
                "anthropic",
                broker_mode.clone(),
                broker_provider,
                &broker_model,
                &broker_base_url,
                ToolRuntimeDecision::FinalAnswer,
                Vec::new(),
                0,
                0,
                broker_started.elapsed().as_millis() as u64,
                &e.to_string(),
            );
            tracing::warn!(error = %e, "工具决策异常，返回主脑文本");
            BrokerDecision::FinalAnswer { usage: None }
        }
    };

    let task_id = format!("msg_{}", uuid_short());
    let elapsed = start_time.elapsed().as_millis() as u64;

    match broker_result {
        BrokerDecision::ToolCalls(calls) => {
            let compiled_calls = calls
                .iter()
                .map(|c| crate::tool_runtime::openai_compiler::CompiledToolCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    input: c.input.clone(),
                })
                .collect::<Vec<_>>();

            // 只有 8.0 才缓存会话用于后续 tool_result，不清理 session
            if let Some((_, session_id)) = reusable_session {
                for call in &compiled_calls {
                    insert_cached_tool_session(
                        &state,
                        &model_full,
                        &short_model,
                        &call.id,
                        session_id,
                    );
                }
            }

            // 记录工具调用到 ledger
            for call in &compiled_calls {
                let key = format!("{}:{}", req.model, call.id);
                if let Err(e) = state
                    .tool_ledger
                    .record_tool_use(&key, &call.id, &call.name)
                {
                    record_anthropic_error(&state, start_time, &model_full, &e, platform_key);
                    return Err(e);
                }

                let broker_call = crate::tool_runtime::broker::BrokerToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                };
                if let Some(plan) = crate::tool_runtime::broker::detect_long_task(&broker_call) {
                    if let Err(e) = state.tool_ledger.attach_long_task(
                        &key,
                        plan.file_path,
                        plan.done_marker,
                        plan.max_polls,
                    ) {
                        record_anthropic_error(&state, start_time, &model_full, &e, platform_key);
                        return Err(e);
                    }
                }
            }

            state.stats.record_success(
                elapsed,
                prompt_tokens,
                completion_tokens,
                &short_model,
                &platform_key,
            );
            state.stats.record_tool_runtime_success(
                &task_id,
                "anthropic",
                broker_mode.clone(),
                broker_provider,
                &broker_model,
                &broker_base_url,
                ToolRuntimeDecision::ToolCalls,
                compiled_calls.iter().map(|c| c.name.clone()).collect(),
                0,
                0,
                broker_started.elapsed().as_millis() as u64,
            );

            let stream = req.stream.unwrap_or(false);
            if stream {
                build_tool_use_stream_response(
                    &short_model,
                    &task_id,
                    compiled_calls,
                    prompt_tokens,
                    completion_tokens,
                )
            } else {
                build_tool_use_response(
                    &short_model,
                    &task_id,
                    compiled_calls,
                    prompt_tokens,
                    completion_tokens,
                )
            }
        }
        BrokerDecision::FinalAnswer { usage } => {
            let latest_user_text = latest_user_plain_text(&req);

            // 判断是否需要补全请求
            let display_text =
                normalize_ouyi_display_text(&ouyi_output, latest_user_text.as_deref());
            let need_supplement = display_text.is_empty() && reusable_session.is_some();

            let (response_text, final_prompt_tokens, final_completion_tokens) = if need_supplement {
                // 补全请求：移动 reusable_session，补全后清理
                let (client, session_id) = reusable_session.unwrap();
                tracing::debug!("主脑展示文本为空，请求 Ouyi 补全最终回答");
                let result =
                    request_final_answer_after_suppressed_tool_intent(&client, session_id, false)
                        .await;
                match result {
                    Ok((final_output, answer_prompt_tokens, answer_completion_tokens)) => {
                        // 补全成功，清理 session
                        state.active_sessions.remove(&session_id);
                        spawn_session_cleanup(client, session_id);
                        (
                            normalize_plain_text(&final_output, latest_user_text.as_deref()),
                            prompt_tokens + answer_prompt_tokens,
                            completion_tokens + answer_completion_tokens,
                        )
                    }
                    Err(e) => {
                        // 补全失败，清理 session 并返回错误
                        state.stats.record_error(
                            elapsed,
                            &model_full,
                            &e.to_string(),
                            &platform_key,
                        );
                        state.active_sessions.remove(&session_id);
                        spawn_session_cleanup(client, session_id);
                        return Err(e);
                    }
                }
            } else {
                // 不需要补全：如果有 session 则清理
                if let Some((client, session_id)) = reusable_session {
                    state.active_sessions.remove(&session_id);
                    spawn_session_cleanup(client, session_id);
                }
                if display_text.is_empty() {
                    (ouyi_output.clone(), prompt_tokens, completion_tokens)
                } else {
                    (display_text, prompt_tokens, completion_tokens)
                }
            };

            state.stats.record_success(
                elapsed,
                final_prompt_tokens,
                final_completion_tokens,
                &short_model,
                &platform_key,
            );
            let (compiler_prompt, compiler_completion) = match &usage {
                Some(u) => (u.prompt_tokens, u.completion_tokens),
                None => (0, 0),
            };
            state.stats.record_tool_runtime_success(
                &task_id,
                "anthropic",
                broker_mode.clone(),
                broker_provider,
                &broker_model,
                &broker_base_url,
                ToolRuntimeDecision::FinalAnswer,
                Vec::new(),
                compiler_prompt,
                compiler_completion,
                broker_started.elapsed().as_millis() as u64,
            );

            let stream = req.stream.unwrap_or(false);
            if stream {
                build_text_stream_response(
                    &short_model,
                    &task_id,
                    &response_text,
                    final_prompt_tokens,
                    final_completion_tokens,
                )
            } else {
                build_text_response(
                    &short_model,
                    &task_id,
                    &response_text,
                    final_prompt_tokens,
                    final_completion_tokens,
                )
            }
        }
    }
}

/// 收集 Ouyi SSE 流的完整输出
async fn collect_ouyi_output(resp: reqwest::Response) -> Result<(String, u64, u64), AppError> {
    use crate::client::sse::{
        OuyiEventType, SseLineBuffer, ouyi_application_error, parse_sse_line,
    };
    use futures::StreamExt;

    let mut content_parts: Vec<String> = Vec::new();
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut sse_buffer = SseLineBuffer::new();
    let mut partial = Vec::new();

    let byte_stream = resp.bytes_stream();
    futures::pin_mut!(byte_stream);

    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = match chunk_result {
            Ok(bytes) => bytes,
            Err(e) => {
                let msg = format!("Ouyi 流读取错误: {}", e);
                tracing::warn!("{}", msg);
                return Err(AppError::SseStream(msg));
            }
        };

        let mut bytes = chunk.to_vec();
        if !partial.is_empty() {
            let mut combined = std::mem::take(&mut partial);
            combined.extend_from_slice(&bytes);
            bytes = combined;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                partial = error.into_bytes();
                continue;
            }
        };
        let lines = sse_buffer.push_chunk(&text);

        for line in lines {
            if let Some(event) = parse_sse_line(&line) {
                if let Some(msg) = ouyi_application_error(&event) {
                    return Err(AppError::Upstream(msg));
                }
                let event_type = OuyiEventType::from(event.event_type.as_str());
                match event_type {
                    OuyiEventType::StringChunk => {
                        if let Some(data) = &event.data
                            && let Some(text) = data.as_str()
                            && !text.is_empty()
                        {
                            content_parts.push(text.to_string());
                        }
                    }
                    OuyiEventType::Object => {
                        if let Some(data) = &event.data
                            && let Ok(usage) = serde_json::from_value::<crate::model::ouyi::OuyiUsage>(
                                data.clone(),
                            )
                        {
                            prompt_tokens = usage.prompt_tokens;
                            completion_tokens = usage.completion_tokens;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(remaining) = sse_buffer.drain_remaining()
        && let Some(event) = parse_sse_line(&remaining)
        && let Some(msg) = ouyi_application_error(&event)
    {
        return Err(AppError::Upstream(msg));
    }

    Ok((
        merge_ouyi_text_parts(&content_parts),
        prompt_tokens,
        completion_tokens,
    ))
}

/// 构建 Anthropic tool_use 非流式响应
fn build_tool_use_response(
    model: &str,
    task_id: &str,
    calls: Vec<crate::tool_runtime::openai_compiler::CompiledToolCall>,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> Result<axum::response::Response, AppError> {
    use crate::model::anthropic::{MessagesResponse, ResponseContentBlock, ResponseUsage};

    let content: Vec<ResponseContentBlock> = calls
        .into_iter()
        .map(|call| ResponseContentBlock::ToolUse {
            id: call.id,
            name: call.name,
            input: call.input,
        })
        .collect();

    let response = MessagesResponse {
        id: task_id.to_string(),
        r#type: "message",
        role: "assistant",
        model: model.to_string(),
        content,
        stop_reason: "tool_use".to_string(),
        usage: ResponseUsage {
            input_tokens: prompt_tokens,
            output_tokens: completion_tokens,
        },
    };

    Ok(axum::Json(response).into_response())
}

/// 构建 Anthropic tool_use 流式响应
fn build_tool_use_stream_response(
    model: &str,
    task_id: &str,
    calls: Vec<crate::tool_runtime::openai_compiler::CompiledToolCall>,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> Result<axum::response::Response, AppError> {
    use crate::model::anthropic::{
        ContentBlockStart, DeltaContent, MessageDeltaInfo, MessageStartInfo, MessageStartMessage,
        ResponseUsage, StreamEvent, serialize_stream_event,
    };

    let mut events: Vec<Bytes> = Vec::new();

    // message_start
    events.push(serialize_stream_event(&StreamEvent::MessageStart {
        message: MessageStartInfo {
            r#type: "message_start",
            message: MessageStartMessage {
                id: task_id.to_string(),
                r#type: "message",
                role: "assistant",
                model: model.to_string(),
                content: vec![],
                stop_reason: None,
                stop_sequence: None,
                usage: ResponseUsage {
                    input_tokens: prompt_tokens,
                    output_tokens: 0,
                },
            },
        },
    }));

    // tool_use blocks
    for (index, call) in calls.into_iter().enumerate() {
        let idx = index as u32;
        events.push(serialize_stream_event(&StreamEvent::ContentBlockStart {
            index: idx,
            content_block: ContentBlockStart::ToolUse {
                id: call.id,
                name: call.name,
                input: serde_json::json!({}),
            },
        }));
        events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
            index: idx,
            delta: DeltaContent::InputJsonDelta {
                partial_json: serde_json::to_string(&call.input).unwrap_or_default(),
            },
        }));
        events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
            index: idx,
        }));
    }

    // message_delta
    events.push(serialize_stream_event(&StreamEvent::MessageDelta {
        delta: MessageDeltaInfo {
            stop_reason: "tool_use".to_string(),
            stop_sequence: None,
        },
        usage: ResponseUsage {
            input_tokens: 0,
            output_tokens: completion_tokens,
        },
    }));

    // message_stop
    events.push(serialize_stream_event(&StreamEvent::MessageStop));

    let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::io::Error>));
    Ok(axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AppError::Internal(e.to_string()))?)
}

/// 构建 Anthropic text 非流式响应
fn build_text_response(
    model: &str,
    task_id: &str,
    text: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> Result<axum::response::Response, AppError> {
    use crate::model::anthropic::{MessagesResponse, ResponseContentBlock, ResponseUsage};

    let response = MessagesResponse {
        id: task_id.to_string(),
        r#type: "message",
        role: "assistant",
        model: model.to_string(),
        content: vec![ResponseContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: "end_turn".to_string(),
        usage: ResponseUsage {
            input_tokens: prompt_tokens,
            output_tokens: completion_tokens,
        },
    };

    Ok(axum::Json(response).into_response())
}

/// 构建 Anthropic text 流式响应
fn build_text_stream_response(
    model: &str,
    task_id: &str,
    text: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> Result<axum::response::Response, AppError> {
    use crate::model::anthropic::{
        ContentBlockStart, DeltaContent, MessageDeltaInfo, MessageStartInfo, MessageStartMessage,
        ResponseUsage, StreamEvent, serialize_stream_event,
    };

    let mut events: Vec<Bytes> = Vec::new();

    // message_start
    events.push(serialize_stream_event(&StreamEvent::MessageStart {
        message: MessageStartInfo {
            r#type: "message_start",
            message: MessageStartMessage {
                id: task_id.to_string(),
                r#type: "message",
                role: "assistant",
                model: model.to_string(),
                content: vec![],
                stop_reason: None,
                stop_sequence: None,
                usage: ResponseUsage {
                    input_tokens: prompt_tokens,
                    output_tokens: 0,
                },
            },
        },
    }));

    // text block
    events.push(serialize_stream_event(&StreamEvent::ContentBlockStart {
        index: 0,
        content_block: ContentBlockStart::Text {
            text: String::new(),
        },
    }));
    events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
        index: 0,
        delta: DeltaContent::TextDelta {
            text: text.to_string(),
        },
    }));
    events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
        index: 0,
    }));

    // message_delta
    events.push(serialize_stream_event(&StreamEvent::MessageDelta {
        delta: MessageDeltaInfo {
            stop_reason: "end_turn".to_string(),
            stop_sequence: None,
        },
        usage: ResponseUsage {
            input_tokens: 0,
            output_tokens: completion_tokens,
        },
    }));

    // message_stop
    events.push(serialize_stream_event(&StreamEvent::MessageStop));

    let stream = futures::stream::iter(events.into_iter().map(Ok::<_, std::io::Error>));
    Ok(axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AppError::Internal(e.to_string()))?)
}

async fn handle_anthropic_stream(
    resp: reqwest::Response,
    model: String,
    session_id: i64,
    client: OuyiHttpClient,
    state: Arc<AppState>,
    cache_key: Option<String>,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let task_id = format!("msg_{}", uuid_short());

    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|r| r.map_err(std::io::Error::other)),
        session_id,
        client,
    );

    let model_for_stats = model.clone();
    let model_for_task = model.clone();
    let task_id_for_task = task_id.clone();

    let state_for_stream = state.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(32);

    tracing::debug!(session_id, "启动 Anthropic 流式响应");

    tokio::spawn(async move {
        let mut converter = AnthropicStreamConverter::new(model_for_task, task_id_for_task);
        let mut sse_buffer = SseLineBuffer::new();
        let mut partial = Vec::new();
        let mut stream_error: Option<String> = None;
        let mut application_error: Option<String> = None;
        let mut downstream_closed = false;

        futures::pin_mut!(guarded);

        let start_evt = converter.start_event();
        tracing::debug!(event_len = start_evt.len(), "发送 message_start");
        if tx.send(start_evt).await.is_err() {
            tracing::warn!("发送 message_start 失败，channel 已关闭");
            return;
        }

        let mut chunk_count = 0u64;
        'read_loop: while let Some(chunk_result) = guarded.next().await {
            let chunk = match chunk_result {
                Ok(bytes) => bytes,
                Err(e) => {
                    let msg = format!("上游字节流读取错误: {}", e);
                    tracing::warn!("{}", msg);
                    stream_error = Some(msg);
                    break;
                }
            };

            if chunk_count % 10 == 0 {
                tracing::debug!(chunk_count, chunk_len = chunk.len(), "读取上游数据块");
            }

            let mut bytes = chunk.to_vec();
            if !partial.is_empty() {
                let mut combined = std::mem::take(&mut partial);
                combined.extend_from_slice(&bytes);
                bytes = combined;
            }
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => {
                    partial = e.into_bytes();
                    continue;
                }
            };

            let lines = sse_buffer.push_chunk(&text);
            for line in lines {
                if let Some(event) = parse_sse_line(&line) {
                    if let Some(msg) = ouyi_application_error(&event) {
                        application_error = Some(msg);
                        break 'read_loop;
                    }
                    let event_type =
                        crate::client::sse::OuyiEventType::from(event.event_type.as_str());
                    let content = event.data.as_ref().and_then(|d| d.as_str()).unwrap_or("");
                    let is_done = event_type == crate::client::sse::OuyiEventType::Done;

                    tracing::debug!(
                        event_type = event.event_type,
                        is_done,
                        content_len = content.len(),
                        "上游事件"
                    );

                    if event_type == crate::client::sse::OuyiEventType::Object {
                        if let Some(data) = &event.data {
                            if let Ok(usage) = serde_json::from_value::<crate::model::ouyi::OuyiUsage>(
                                data.clone(),
                            ) {
                                converter
                                    .update_usage(usage.prompt_tokens, usage.completion_tokens);
                            }
                        }
                    }

                    for evt in converter.process_ouyi_event(content, is_done) {
                        let evt_str = String::from_utf8_lossy(&evt);
                        let preview: String = evt_str.chars().take(120).collect();
                        tracing::debug!(preview = %preview, "发送 Anthropic SSE 事件");
                        if tx.send(evt).await.is_err() {
                            tracing::warn!("发送 SSE 事件失败，channel 已关闭");
                            downstream_closed = true;
                            break 'read_loop;
                        }
                    }
                }
            }
            chunk_count += 1;
        }

        if application_error.is_none()
            && stream_error.is_none()
            && let Some(remaining) = sse_buffer.drain_remaining()
            && let Some(event) = parse_sse_line(&remaining)
        {
            application_error = ouyi_application_error(&event);
        }

        if downstream_closed {
            state_for_stream.active_sessions.remove(&session_id);
            return;
        }

        if let Some(msg) = application_error {
            state_for_stream.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_for_stats,
                &msg,
                "ouyi8",
            );
            state_for_stream.active_sessions.remove(&session_id);
            if let Some(key) = &cache_key {
                state_for_stream.tool_sessions.remove(key);
            }
            let error = serde_json::json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": msg
                }
            });
            let payload = format!("event: error\ndata: {}\n\n", error);
            let _ = tx.send(Bytes::from(payload)).await;
            return;
        }

        if let Some(msg) = stream_error {
            state_for_stream.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_for_stats,
                &msg,
                "ouyi8",
            );
            state_for_stream.active_sessions.remove(&session_id);
            let error = serde_json::json!({
                "type": "error",
                "error": {
                    "type": "stream_error",
                    "message": msg
                }
            });
            let payload = format!("event: error\ndata: {}\n\n", error);
            let _ = tx.send(Bytes::from(payload)).await;
            return;
        }

        if !converter.done_processed {
            tracing::debug!("上游流结束，发送 done 事件");
            for evt in converter.process_ouyi_event("", true) {
                let _ = tx.send(evt).await;
            }
        }

        tracing::info!(
            session_id,
            model = %model_for_stats,
            chunks = chunk_count,
            has_tool_use = converter.has_tool_use,
            "流式处理完成"
        );

        // 根据 has_tool_use 决定缓存或清理会话
        let has_tool_use = converter.has_tool_use;

        let stream_elapsed = start_time.elapsed().as_millis() as u64;
        let (input_tokens, output_tokens) = converter.tokens();
        state_for_stream.stats.record_success(
            stream_elapsed,
            input_tokens,
            output_tokens,
            &model_for_stats,
            "ouyi8",
        );

        if has_tool_use {
            // tool_use 响应：阻止 GuardedStream Drop 自动删除会话
            guarded.as_mut().skip_cleanup();

            // 缓存会话 ID 供下一轮 tool_result 复用
            let last_tool_id = converter.last_tool_use_id.clone();
            if let Some(tool_id) = last_tool_id {
                let key = global_tool_session_cache_key(&tool_id);
                state_for_stream.tool_sessions.insert(
                    key,
                    crate::CachedToolSession {
                        session_id,
                        created_at: std::time::Instant::now(),
                    },
                );
                tracing::debug!(session_id, tool_use_id = %tool_id, "缓存工具会话等待 tool_result 回传");
            }
        } else {
            // end_turn：清理会话和可能的旧缓存
            state_for_stream.active_sessions.remove(&session_id);
            if let Some(key) = &cache_key {
                state_for_stream.tool_sessions.remove(key);
            }
        }
    });

    // 流式模式：立即返回响应，不在此处删除会话
    // 会话的清理/缓存在 spawn 的 task 中根据 has_tool_use 决定

    // 确保 hs 日志能在调试时被看到
    tracing::debug!(session_id, "Anthropic SSE 流构建完成，返回响应");

    let stream = ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(stream.map(|b| Ok::<_, std::io::Error>(b)));
    let response = axum::response::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("anthropic-version", "2023-06-01")
        .header("x-request-id", task_id)
        .body(body)
        .unwrap();

    Ok(response)
}

async fn handle_anthropic_non_stream(
    resp: reqwest::Response,
    model: String,
    session_id: i64,
    client: OuyiHttpClient,
    state: Arc<AppState>,
    cache_key: Option<String>,
    elapsed: u64,
) -> Result<axum::response::Response, AppError> {
    let task_id = format!("msg_{}", uuid_short());
    let mut collector = AnthropicCollector::new(model.clone(), task_id.clone());
    let mut sse_buffer = SseLineBuffer::new();

    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|r| r.map_err(std::io::Error::other)),
        session_id,
        client.clone(),
    );

    futures::pin_mut!(guarded);

    let mut total_bytes: usize = 0;
    let max_bytes = state.config.read().await.limits.max_response_bytes;
    let mut partial = Vec::new();
    let mut stream_error: Option<String> = None;
    let mut application_error: Option<String> = None;

    'read_loop: while let Some(chunk_result) = guarded.next().await {
        let chunk = match chunk_result {
            Ok(bytes) => bytes,
            Err(e) => {
                let msg = format!("SSE字节流读取错误: {}", e);
                tracing::error!(error = %e, "SSE字节流读取错误");
                stream_error = Some(msg);
                break;
            }
        };

        total_bytes += chunk.len();
        if total_bytes > max_bytes {
            stream_error = Some(format!("响应超过大小限制: {} > {}", total_bytes, max_bytes));
            break;
        }

        let mut bytes = chunk.to_vec();
        if !partial.is_empty() {
            let mut combined = std::mem::take(&mut partial);
            combined.extend_from_slice(&bytes);
            bytes = combined;
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                partial = e.into_bytes();
                continue;
            }
        };

        let lines = sse_buffer.push_chunk(&text);
        for line in lines {
            if let Some(event) = parse_sse_line(&line) {
                if let Some(msg) = ouyi_application_error(&event) {
                    application_error = Some(msg);
                    break 'read_loop;
                }
                collector.push_event(&event);
            }
        }
    }

    if application_error.is_none()
        && let Some(remaining) = sse_buffer.drain_remaining()
        && let Some(event) = parse_sse_line(&remaining)
    {
        if let Some(msg) = ouyi_application_error(&event) {
            application_error = Some(msg);
        } else {
            collector.push_event(&event);
        }
    }

    if let Some(msg) = application_error {
        guarded.as_mut().skip_cleanup();
        spawn_session_cleanup(client, session_id);
        state.active_sessions.remove(&session_id);
        if let Some(key) = &cache_key {
            state.tool_sessions.remove(key);
        }
        state.stats.record_error(elapsed, &model, &msg, "ouyi8");
        return Err(AppError::Upstream(msg));
    }

    if let Some(msg) = stream_error {
        guarded.as_mut().skip_cleanup();
        spawn_session_cleanup(client, session_id);
        state.active_sessions.remove(&session_id);
        if let Some(key) = &cache_key {
            state.tool_sessions.remove(key);
        }
        state.stats.record_error(elapsed, &model, &msg, "ouyi8");
        return Err(AppError::SseStream(msg));
    }

    let (prompt, completion) = collector.tokens();
    let response = collector.build_response();

    state
        .stats
        .record_success(elapsed, prompt, completion, &model, "ouyi8");

    // 根据 stop_reason 决定缓存或清理会话
    let has_tool_use = response.stop_reason == "tool_use";
    if has_tool_use {
        guarded.as_mut().skip_cleanup();
        // 从响应中提取最后一个 tool_use 的 id 作为缓存 key
        let last_tool_id = response.content.iter().rev().find_map(|b| {
            if let crate::model::anthropic::ResponseContentBlock::ToolUse { id, .. } = b {
                Some(id.clone())
            } else {
                None
            }
        });
        if let Some(tool_id) = last_tool_id {
            let key = global_tool_session_cache_key(&tool_id);
            state.tool_sessions.insert(
                key,
                crate::CachedToolSession {
                    session_id,
                    created_at: std::time::Instant::now(),
                },
            );
            tracing::info!(session_id, tool_use_id = %tool_id, "缓存工具会话等待 tool_result 回传");
        }
        // 不删除会话，保留给下一轮
    } else {
        guarded.as_mut().skip_cleanup();
        spawn_session_cleanup(client, session_id);
        state.active_sessions.remove(&session_id);
        if let Some(key) = &cache_key {
            state.tool_sessions.remove(key);
        }
    }

    let mut resp = Json(response).into_response();
    resp.headers_mut()
        .insert("anthropic-version", "2023-06-01".parse().unwrap());
    resp.headers_mut()
        .insert("x-request-id", task_id.parse().unwrap());
    Ok(resp)
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", ts % 0xFFFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::routing::post;
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::admin::stats::StatsCollector;
    use crate::admin::{AccountStore, auth::AdminStore};
    use crate::client::http::OuyiHttpClient;
    use crate::config::{
        AdminConfig, AppConfig, LimitsConfig, Ouyi7Config, OuyiConfig, PlatformConfig,
        ServerConfig, ToolCallConfig, ToolRuntimeConfig, ToolRuntimeMode, ToolRuntimeProvider,
    };
    use crate::model::anthropic::{
        AnthropicContent, AnthropicMessage, AnthropicTool, ContentBlock,
    };
    use crate::model::mapper::ModelMapper;

    fn make_tool_result_msg(tool_use_id: &str, content: &str) -> AnthropicMessage {
        AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error: false,
            }]),
        }
    }

    fn make_messages_request(messages: Vec<AnthropicMessage>) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages,
            system: None,
            max_tokens: 4096,
            stream: Some(true),
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        }
    }

    #[test]
    fn test_format_tool_results_for_reuse_single() {
        let tool_result = make_tool_result_msg("call_0", "file1.rs\nfile2.rs");
        let req = make_messages_request(vec![tool_result]);

        let result = format_tool_results_for_reuse(&req);
        assert!(result.contains("call_0"));
        assert!(result.contains("file1.rs"));
        assert!(result.contains("file2.rs"));
        assert!(result.contains("请根据以上工具执行结果继续"));
        assert!(!result.contains("直接输出工具调用标记"));
        // 不应包含工具指令
        assert!(!result.contains("You have access to the following tools"));
        assert!(!result.contains("[System]"));
    }

    #[test]
    fn test_format_tool_results_for_reuse_multiple() {
        let msg = AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_0".to_string(),
                    content: "result A".to_string(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "result B".to_string(),
                    is_error: false,
                },
            ]),
        };
        let req = make_messages_request(vec![msg]);

        let result = format_tool_results_for_reuse(&req);
        assert!(result.contains("call_0"));
        assert!(result.contains("call_1"));
        assert!(result.contains("result A"));
        assert!(result.contains("result B"));
    }

    #[test]
    fn test_format_tool_results_for_reuse_preserves_typical_file_output() {
        let long_content = "x".repeat(3000);
        let tool_result = make_tool_result_msg("call_0", &long_content);
        let req = make_messages_request(vec![tool_result]);

        let result = format_tool_results_for_reuse(&req);
        assert!(!result.contains("截断"));
        assert!(result.contains(&long_content));
    }

    #[test]
    fn test_format_tool_results_for_reuse_preserves_typical_multibyte_file_output() {
        let long_content = "方".repeat(1000);
        let tool_result = make_tool_result_msg("call_0", &long_content);
        let req = make_messages_request(vec![tool_result]);

        let result = format_tool_results_for_reuse(&req);

        assert!(!result.contains("截断"));
        assert!(result.contains(&long_content));
    }

    #[test]
    fn test_format_tool_results_for_reuse_truncates_extreme_output_with_notice() {
        let long_content = "x".repeat(TOOL_RESULT_CONTENT_MAX_BYTES + 1);
        let tool_result = make_tool_result_msg("call_0", &long_content);
        let req = make_messages_request(vec![tool_result]);

        let result = format_tool_results_for_reuse(&req);

        assert!(result.contains(&format!(
            "内容截断，共 {} 字符",
            TOOL_RESULT_CONTENT_MAX_BYTES + 1
        )));
    }

    #[test]
    fn test_format_tool_results_for_reuse_empty() {
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::Text {
                text: "no tools here".to_string(),
            }]),
        };
        let req = make_messages_request(vec![msg]);

        let result = format_tool_results_for_reuse(&req);
        assert!(result.is_empty());
    }

    #[test]
    fn test_has_tool_result_positive() {
        let msg = make_tool_result_msg("call_0", "result");
        let req = make_messages_request(vec![msg]);
        assert!(has_tool_result(&req));
    }

    #[test]
    fn test_has_tool_result_negative() {
        let msg = AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        };
        let req = make_messages_request(vec![msg]);
        assert!(!has_tool_result(&req));
    }

    #[test]
    fn test_extract_last_tool_use_id() {
        let msg = AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_0".to_string(),
                    content: "first".to_string(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "last".to_string(),
                    is_error: false,
                },
            ]),
        };
        let req = make_messages_request(vec![msg]);
        assert_eq!(extract_last_tool_use_id(&req), Some("call_1".to_string()));
    }

    #[test]
    fn test_tool_count_treats_empty_tools_as_zero() {
        let mut req = make_messages_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }]);

        req.tools = None;
        assert_eq!(tool_count(&req), 0);

        req.tools = Some(vec![]);
        assert_eq!(tool_count(&req), 0);

        req.tools = Some(vec![bash_tool()]);
        assert_eq!(tool_count(&req), 1);
    }

    #[test]
    fn test_merge_ouyi_text_parts_uses_latest_cumulative_snapshot() {
        assert_eq!(
            merge_ouyi_text_parts(&[
                "你好".to_string(),
                "你好！有什么我可以帮你的吗？".to_string(),
                "你好！有什么我可以帮你的吗？".to_string(),
            ]),
            "你好！有什么我可以帮你的吗？".to_string()
        );
    }

    #[test]
    fn test_merge_ouyi_text_parts_keeps_incremental_chunks() {
        assert_eq!(
            merge_ouyi_text_parts(&["第一段".to_string(), "\n\n第二段".to_string()]),
            "第一段\n\n第二段".to_string()
        );
    }

    #[test]
    fn test_normalize_plain_text_removes_prefix_snapshot_and_duplicate_answer() {
        assert_eq!(
            normalize_plain_text(
                "你好\n\n你好！有什么我可以帮你的吗？\n\n你好！有什么我可以帮你的吗？",
                Some("你好"),
            ),
            "你好！有什么我可以帮你的吗？".to_string()
        );
    }

    #[test]
    fn test_normalize_plain_text_handles_line_based_echo_and_duplicate_answer() {
        assert_eq!(
            normalize_plain_text(
                "你好\n你好！有什么我可以帮你的吗？\n你好！有什么我可以帮你的吗？",
                Some("你好"),
            ),
            "你好！有什么我可以帮你的吗？".to_string()
        );
    }

    // runtime_route tests
    fn make_tool() -> crate::model::anthropic::AnthropicTool {
        crate::model::anthropic::AnthropicTool {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn make_request_with_tools() -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("test".to_string()),
            }],
            system: None,
            max_tokens: 1024,
            stream: Some(true),
            tools: Some(vec![make_tool()]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: None,
        }
    }

    fn make_test_state(config: AppConfig) -> Arc<AppState> {
        let http_client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();
        let platform = config
            .platform
            .as_ref()
            .map(|p| p.default)
            .unwrap_or(crate::platform::OuyiPlatform::Ouyi8);
        let base_url = config.ouyi.base_url.clone();
        Arc::new(AppState {
            concurrency_limiter: crate::concurrency::ConcurrencyLimiter::new(0),
            ouyi_client: tokio::sync::RwLock::new(crate::client::OuyiClient::V8(
                OuyiHttpClient::new(
                    http_client.clone(),
                    config.ouyi.base_url.clone(),
                    config.ouyi.jwt_token.clone(),
                    config.ouyi.app_version.clone(),
                ),
            )),
            config: tokio::sync::RwLock::new(config),
            http_client,
            active_sessions: dashmap::DashMap::new(),
            tool_sessions: dashmap::DashMap::new(),
            tool_ledger: crate::tool_runtime::ToolLedger::new(),
            responses_state: crate::responses_state::ResponsesState::default(),
            model_mapper: mapper,
            admin_store: tokio::sync::RwLock::new(AdminStore::new(
                "test-secret".to_string(),
                String::new(),
            )),
            account_store: AccountStore::new(),
            stats: StatsCollector::new(),
            platform: tokio::sync::RwLock::new(platform),
            platform_status: tokio::sync::RwLock::new(crate::platform::PlatformStatus::new(
                platform, base_url,
            )),
        })
    }

    fn make_v7_test_state(
        ouyi_base_url: String,
        tool_runtime: Option<ToolRuntimeConfig>,
    ) -> Arc<AppState> {
        use crate::client::ouyi_v7::Ouyi7HttpClient;

        let http_client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();
        let platform = OuyiPlatform::Ouyi7;
        let config = AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
            },
            ouyi: OuyiConfig {
                base_url: ouyi_base_url.clone(),
                jwt_token: "test-jwt".to_string(),
                app_version: "3.0.3".to_string(),
                model_refresh_interval_secs: 3600,
            },
            limits: LimitsConfig {
                request_timeout_secs: 5,
                no_data_timeout_secs: 5,
                max_response_bytes: 1024 * 1024,
                max_messages_bytes: 1024 * 1024,
                max_concurrent_requests: 0,
            },
            admin: Some(AdminConfig {
                jwt_secret: Some("test-secret".to_string()),
                password_hash: None,
            }),
            tool_call: ToolCallConfig::default(),
            tool_runtime,
            openai_tool_runtime: None,
            platform: Some(PlatformConfig {
                default: platform,
                prompt_on_start: false,
            }),
            ouyi_v7: Some(Ouyi7Config {
                base_url: ouyi_base_url.clone(),
                chatapi_prefix: "/chatapi".to_string(),
                jwt_token: "test-v7-jwt".to_string(),
                default_model: "ouyi-chat".to_string(),
                model_refresh_interval_secs: 3600,
                max_history_messages: 20,
                simulated_stream_chunk_chars: 80,
            }),
        };

        Arc::new(AppState {
            concurrency_limiter: crate::concurrency::ConcurrencyLimiter::new(0),
            ouyi_client: tokio::sync::RwLock::new(crate::client::OuyiClient::V7(
                Ouyi7HttpClient::new(
                    http_client.clone(),
                    ouyi_base_url.clone(),
                    "/chatapi".to_string(),
                    "test-v7-jwt".to_string(),
                ),
            )),
            config: tokio::sync::RwLock::new(config),
            http_client,
            active_sessions: dashmap::DashMap::new(),
            tool_sessions: dashmap::DashMap::new(),
            tool_ledger: crate::tool_runtime::ToolLedger::new(),
            responses_state: crate::responses_state::ResponsesState::default(),
            model_mapper: mapper,
            admin_store: tokio::sync::RwLock::new(AdminStore::new(
                "test-secret".to_string(),
                String::new(),
            )),
            account_store: AccountStore::new(),
            stats: StatsCollector::new(),
            platform: tokio::sync::RwLock::new(platform),
            platform_status: tokio::sync::RwLock::new(crate::platform::PlatformStatus::new(
                platform,
                ouyi_base_url,
            )),
        })
    }

    fn test_config(ouyi_base_url: String, tool_base_url: String) -> AppConfig {
        AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
            },
            ouyi: OuyiConfig {
                base_url: ouyi_base_url,
                jwt_token: "test-jwt".to_string(),
                app_version: "3.0.3".to_string(),
                model_refresh_interval_secs: 3600,
            },
            limits: LimitsConfig {
                request_timeout_secs: 5,
                no_data_timeout_secs: 5,
                max_response_bytes: 1024 * 1024,
                max_messages_bytes: 1024 * 1024,
                max_concurrent_requests: 0,
            },
            admin: Some(AdminConfig {
                jwt_secret: Some("test-secret".to_string()),
                password_hash: None,
            }),
            tool_call: ToolCallConfig::default(),
            openai_tool_runtime: None,
            tool_runtime: Some(ToolRuntimeConfig {
                enabled: true,
                mode: ToolRuntimeMode::Compiler,
                provider: ToolRuntimeProvider::OpenaiCompatible,
                base_url: Some(tool_base_url),
                api_key: Some("sk-test".to_string()),
                api_key_env: None,
                model: Some("tool-model".to_string()),
                timeout_secs: 5,
            }),
            platform: None,
            ouyi_v7: None,
        }
    }

    async fn spawn_mock_router(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{}", addr)
    }

    fn bash_tool() -> AnthropicTool {
        AnthropicTool {
            name: "Bash".to_string(),
            description: "Run a shell command".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        }
    }

    async fn ouyi_session_handler() -> axum::Json<serde_json::Value> {
        axum::Json(json!({
            "code": 0,
            "msg": "ok",
            "data": {
                "id": 42,
                "model": "claude_chat::claude-sonnet-4-5-20250929-vip",
                "contextCount": 0,
                "temperature": 0.0
            }
        }))
    }

    fn ouyi_sse_response(output: &str) -> axum::response::Response {
        let body = format!(
            "data: {{\"id\":\"1\",\"type\":\"string\",\"data\":{}}}\n\n\
             data: {{\"id\":\"1\",\"type\":\"object\",\"data\":{{\"promptTokens\":7,\"completionTokens\":5}}}}\n\n\
             data: {{\"id\":\"1\",\"type\":\"done\"}}\n\n",
            serde_json::to_string(output).unwrap()
        );
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }

    async fn ouyi_chat_handler() -> axum::response::Response {
        ouyi_sse_response(concat!(
            "<tool_use>\n",
            "{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}\n",
            "</tool_use>"
        ))
    }

    async fn ouyi_plain_hello_handler() -> axum::response::Response {
        ouyi_sse_response("你好！有什么我可以帮你的吗？")
    }

    async fn ouyi_plain_tool_intent_handler() -> axum::response::Response {
        ouyi_sse_response("需要执行命令确认当前目录。")
    }

    async fn ouyi_plain_duplicate_hello_handler() -> axum::response::Response {
        ouyi_sse_response("你好\n\n你好！有什么我可以帮你的吗？\n\n你好！有什么我可以帮你的吗？")
    }

    async fn ouyi_application_error_handler() -> axum::response::Response {
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"id\":\"quota\",\"type\":\"\",\"data\":null,\"err\":\"您的套餐余量不足，请购买后重试\",\"code\":1}\n\n",
                "data: [DONE]\n\n"
            )))
            .unwrap()
    }

    async fn ouyi_split_utf8_application_error_handler() -> axum::response::Response {
        let body = concat!(
            "data: {\"id\":\"quota\",\"type\":\"\",\"data\":null,\"err\":\"您的套餐余量不足，请购买后重试\",\"code\":1}\n\n",
            "data: [DONE]\n\n"
        );
        let split_at = body.find("套").unwrap() + 1;
        let chunks = vec![
            Ok::<_, std::convert::Infallible>(bytes::Bytes::copy_from_slice(
                &body.as_bytes()[..split_at],
            )),
            Ok(bytes::Bytes::copy_from_slice(&body.as_bytes()[split_at..])),
        ];

        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(tokio_stream::iter(chunks)))
            .unwrap()
    }

    async fn ouyi_tool_then_final_handler(
        State(counter): State<Arc<AtomicUsize>>,
    ) -> axum::response::Response {
        let request_index = counter.fetch_add(1, Ordering::SeqCst);
        if request_index == 0 {
            ouyi_sse_response(concat!(
                "<tool_use>\n",
                "{\"name\":\"Bash\",\"arguments\":{\"command\":\"Get-Process\"}}\n",
                "</tool_use>"
            ))
        } else {
            ouyi_sse_response("内存占用较高的进程需要结合工具结果继续判断。")
        }
    }

    async fn ouyi7_mock_router_with_reply(reply: &'static str) -> Router {
        async fn save_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "result": {
                    "id": 12345
                }
            }))
        }

        async fn message_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "result": [11111, 22222]
            }))
        }

        async fn message_response_handler(
            State(reply): State<&'static str>,
        ) -> axum::response::Response {
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "text/plain")
                .body(Body::from(reply))
                .unwrap()
        }

        async fn delete_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "result": true
            }))
        }

        Router::new()
            .route("/chatapi/chat/save", post(save_handler))
            .route("/chatapi/chat/message", post(message_handler))
            .route("/chatapi/chat/message/{id}", post(message_response_handler))
            .route("/chatapi/chat/delete", post(delete_handler))
            .with_state(reply)
    }

    async fn tool_compiler_handler(
        State(counter): State<Arc<AtomicUsize>>,
    ) -> axum::Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "tool_call_from_compiler",
                        "type": "function",
                        "function": {
                            "name": "Bash",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 3
            }
        }))
    }

    #[tokio::test]
    async fn test_anthropic_non_stream_rejects_ouyi_application_error() {
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/completions",
                    post(ouyi_application_error_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(Router::new()).await;
        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_messages_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }]);
        req.stream = Some(false);

        let err = messages(State(state), Json(req)).await.unwrap_err();

        assert!(
            matches!(err, AppError::Upstream(message) if message.contains("您的套餐余量不足，请购买后重试"))
        );
    }

    #[tokio::test]
    async fn test_anthropic_stream_emits_error_without_stop_for_ouyi_application_error() {
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/completions",
                    post(ouyi_application_error_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(Router::new()).await;
        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let req = make_messages_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }]);

        let response = messages(State(state), Json(req)).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_text = String::from_utf8(body.to_vec()).unwrap();

        assert!(body_text.contains("event: error"));
        assert!(body_text.contains(r#""type":"api_error""#));
        assert!(body_text.contains("您的套餐余量不足，请购买后重试"));
        assert!(!body_text.contains("message_stop"));
    }

    #[tokio::test]
    async fn test_anthropic_tools_reject_ouyi_application_error_before_compiler() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/completions",
                    post(ouyi_application_error_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_handler))
                .with_state(tool_requests.clone()),
        )
        .await;
        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);

        let err = messages(State(state), Json(req)).await.unwrap_err();

        assert!(
            matches!(err, AppError::Upstream(message) if message.contains("您的套餐余量不足，请购买后重试"))
        );
        assert_eq!(tool_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_anthropic_tools_preserve_split_utf8_application_error() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/completions",
                    post(ouyi_split_utf8_application_error_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_handler))
                .with_state(tool_requests.clone()),
        )
        .await;
        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);

        let err = messages(State(state), Json(req)).await.unwrap_err();
        let AppError::Upstream(message) = err else {
            panic!("expected upstream error");
        };

        assert_eq!(message, "您的套餐余量不足，请购买后重试 (code=1)");
        assert!(!message.contains('�'));
        assert_eq!(tool_requests.load(Ordering::SeqCst), 0);
    }

    async fn tool_compiler_final_handler(
        State(counter): State<Arc<AtomicUsize>>,
    ) -> axum::Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "choices": [{
                "message": {
                    "content": "不需要继续调用工具"
                }
            }],
            "usage": {
                "prompt_tokens": 13,
                "completion_tokens": 2
            }
        }))
    }

    #[tokio::test]
    async fn test_ouyi7_anthropic_messages_supports_tools() {
        let mock_router = ouyi7_mock_router_with_reply(concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        ))
        .await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state(base_url, None);

        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_ouyi7_message(state, req, routing).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["content"][0]["type"], "tool_use");
        assert_eq!(value["content"][0]["name"], "Bash");
        assert_eq!(value["usage"]["input_tokens"], 0);
        assert_eq!(value["usage"]["output_tokens"], 0);
    }

    #[tokio::test]
    async fn test_ouyi7_anthropic_tools_final_answer_without_compiler_config() {
        let mock_router = ouyi7_mock_router_with_reply("你好！有什么我可以帮你的吗？").await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state(base_url, None);

        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_ouyi7_message(state, req, routing).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stop_reason"], "end_turn");
        assert_eq!(value["content"][0]["type"], "text");
        assert!(
            value["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("你好")
        );
        assert_eq!(value["usage"]["input_tokens"], 0);
        assert_eq!(value["usage"]["output_tokens"], 0);
    }

    #[tokio::test]
    async fn test_ouyi7_anthropic_tool_result_continues_to_final_text() {
        let mock_router = ouyi7_mock_router_with_reply("目录里有 Cargo.toml。").await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state(base_url, None);

        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "Bash".to_string(),
                input: json!({"command": "pwd"}),
            }]),
        });
        req.messages.push(make_tool_result_msg(
            "toolu_1",
            "D:\\document\\Projects\\ouyi-api",
        ));
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_ouyi7_message(state, req, routing).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stop_reason"], "end_turn");
        assert!(
            value["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Cargo.toml")
        );
        assert_eq!(value["usage"]["input_tokens"], 0);
        assert_eq!(value["usage"]["output_tokens"], 0);
    }

    #[tokio::test]
    async fn test_compiler_route_invokes_tool_model_even_when_ouyi_output_is_parseable() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route("/api/chat/completions", post(ouyi_chat_handler)),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_handler))
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state.clone(), req, routing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // 标记格式工具调用被 broker 直接解析，不经过工具模型编译器
        assert_eq!(tool_requests.load(Ordering::SeqCst), 0);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["content"][0]["type"], "tool_use");
        assert_eq!(value["content"][0]["name"], "Bash");
    }

    #[tokio::test]
    async fn test_plain_ouyi_text_uses_tool_model_final_answer() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route("/api/chat/completions", post(ouyi_plain_hello_handler)),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_final_handler))
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state.clone(), req, routing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(tool_requests.load(Ordering::SeqCst), 1);
        assert!(state.stats.get_model_stats().iter().any(
            |(model, requests, prompt, completion)| model == "tool-runtime::tool-model"
                && *requests == 1
                && *prompt == 13
                && *completion == 2
        ));
        assert_eq!(state.stats.get_tool_runtime_logs(10).len(), 1);
    }

    #[tokio::test]
    async fn test_plain_tool_intent_invokes_tool_model() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/completions",
                    post(ouyi_plain_tool_intent_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_handler))
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state, req, routing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(tool_requests.load(Ordering::SeqCst), 1);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["content"][0]["type"], "tool_use");
        assert_eq!(value["content"][0]["name"], "Bash");
    }

    #[tokio::test]
    async fn test_plain_ouyi_stream_collapses_repeated_full_text() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/completions",
                    post(ouyi_plain_duplicate_hello_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_final_handler))
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(true);
        req.tools = Some(vec![bash_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state, req, routing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_text = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(tool_requests.load(Ordering::SeqCst), 1);
        assert_eq!(body_text.matches("你好！有什么我可以帮你的吗？").count(), 1);
        assert!(!body_text.contains(r#""text":"你好""#));
    }

    #[tokio::test]
    async fn test_compiler_final_answer_reasks_ouyi_when_display_text_is_empty() {
        let ouyi_requests = Arc::new(AtomicUsize::new(0));
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route("/api/chat/completions", post(ouyi_tool_then_final_handler))
                .with_state(ouyi_requests.clone()),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_final_handler))
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        req.messages.push(make_tool_result_msg(
            "tool_call_from_compiler",
            "Get-Process output",
        ));
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state, req, routing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(ouyi_requests.load(Ordering::SeqCst), 1);
        assert_eq!(tool_requests.load(Ordering::SeqCst), 0);
        assert_eq!(value["content"][0]["type"], "tool_use");
    }

    #[tokio::test]
    async fn test_tool_session_cache_is_available_by_short_and_full_model_names() {
        let state = make_test_state(test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        ));
        let full_model = "claude_chat::claude-sonnet-4-5-20250929-vip";
        let short_model = "claude-sonnet-4-5";

        insert_cached_tool_session(&state, full_model, short_model, "call_9", 42);

        let cached = take_cached_tool_session(&state, short_model, full_model, "call_9")
            .expect("short/full model cache aliases should resolve to the same session");

        assert_eq!(cached.1.session_id, 42);
        assert!(state.tool_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_tool_session_cache_survives_request_model_change() {
        let state = make_test_state(test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        ));
        let full_model = "claude_chat::claude-sonnet-4-5-20250929-vip";
        let short_model = "claude-sonnet-4-5";
        let different_model = "claude-opus-4-7";

        insert_cached_tool_session(&state, full_model, short_model, "call_9", 42);

        let cached = take_cached_tool_session(&state, different_model, different_model, "call_9")
            .expect("global key should allow cache lookup across models");

        assert_eq!(cached.1.session_id, 42);
        assert!(state.tool_sessions.is_empty());
    }

    #[tokio::test]
    async fn test_pending_long_task_poll_response_registers_returned_tool_use_id() {
        let state = make_test_state(test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        ));

        state
            .tool_ledger
            .record_tool_use("claude-test:call_start", "call_start", "Bash")
            .unwrap();
        state
            .tool_ledger
            .attach_long_task(
                "claude-test:call_start",
                "longtask.txt".to_string(),
                "DONE_MARKER".to_string(),
                3,
            )
            .unwrap();

        let mut req = make_request_with_tools();
        req.model = "claude-test".to_string();
        req.stream = Some(false);
        req.tools = Some(vec![bash_tool()]);
        req.messages
            .push(make_tool_result_msg("call_start", "PENDING"));

        let response = maybe_build_long_task_poll_response(&state, &req, &req.model)
            .unwrap()
            .expect("pending long task should produce poll response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let poll_id = value["content"][0]["id"].as_str().unwrap();

        assert!(
            state
                .tool_ledger
                .long_task_for_tool_use(poll_id, "claude-test")
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_long_task_poll_closes_across_multiple_tool_results() {
        let state = make_test_state(test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        ));

        state
            .tool_ledger
            .record_tool_use("claude-test:call_start", "call_start", "Bash")
            .unwrap();
        let task_id = state
            .tool_ledger
            .attach_long_task(
                "claude-test:call_start",
                "longtask.txt".to_string(),
                "DONE_MARKER".to_string(),
                5,
            )
            .unwrap();

        let mut req1 = make_request_with_tools();
        req1.model = "claude-test".to_string();
        req1.stream = Some(false);
        req1.tools = Some(vec![bash_tool()]);
        req1.messages
            .push(make_tool_result_msg("call_start", "PENDING"));

        let resp1 = maybe_build_long_task_poll_response(&state, &req1, &req1.model)
            .unwrap()
            .expect("first pending result should produce poll response");
        let body1 = resp1.into_body().collect().await.unwrap().to_bytes();
        let value1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
        let poll1 = value1["content"][0]["id"].as_str().unwrap().to_string();
        assert!(
            state
                .tool_ledger
                .long_task_for_tool_use(&poll1, "claude-test")
                .is_some()
        );

        let mut req2 = make_request_with_tools();
        req2.model = "claude-test".to_string();
        req2.stream = Some(false);
        req2.tools = Some(vec![bash_tool()]);
        req2.messages
            .push(make_tool_result_msg(&poll1, "still running"));

        let resp2 = maybe_build_long_task_poll_response(&state, &req2, &req2.model)
            .unwrap()
            .expect("second pending result should produce poll response");
        let body2 = resp2.into_body().collect().await.unwrap().to_bytes();
        let value2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        let poll2 = value2["content"][0]["id"].as_str().unwrap().to_string();
        assert!(
            state
                .tool_ledger
                .long_task_for_tool_use(&poll2, "claude-test")
                .is_some()
        );

        let mut req3 = make_request_with_tools();
        req3.model = "claude-test".to_string();
        req3.stream = Some(false);
        req3.tools = Some(vec![bash_tool()]);
        req3.messages
            .push(make_tool_result_msg(&poll2, "DONE_MARKER"));

        let done = maybe_build_long_task_poll_response(&state, &req3, &req3.model).unwrap();
        assert!(done.is_none());
        assert!(
            state
                .tool_ledger
                .long_task_for_tool_use("call_start", "claude-test")
                .is_none()
        );
        assert!(
            state
                .tool_ledger
                .long_task_for_tool_use(&poll1, "claude-test")
                .is_none()
        );
        assert!(
            state
                .tool_ledger
                .long_task_for_tool_use(&poll2, "claude-test")
                .is_none()
        );
        assert!(state.tool_ledger.long_task_by_id(&task_id).is_none());
    }

    fn make_anthropic_config() -> crate::config::ToolRuntimeConfig {
        crate::config::ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: crate::config::ToolRuntimeProvider::Anthropic,
            base_url: Some("https://api.anthropic.com".to_string()),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            model: Some("claude-sonnet-4-7".to_string()),
            timeout_secs: 120,
        }
    }

    fn make_compiler_config() -> crate::config::ToolRuntimeConfig {
        crate::config::ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: crate::config::ToolRuntimeProvider::OpenaiCompatible,
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            model: Some("gpt-4".to_string()),
            timeout_secs: 120,
        }
    }

    #[tokio::test]
    async fn test_anthropic_compiler_missing_jwt_records_error_and_clears_active_request() {
        let mut config = test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        );
        config.ouyi.jwt_token.clear();
        let state = make_test_state(config);
        let req = make_request_with_tools();
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let err = handle_compiler_tool_runtime_message(state.clone(), req, routing)
            .await
            .unwrap_err();

        assert!(matches!(err, AppError::BadRequest(_)));
        assert_eq!(state.stats.stats.active_requests.load(Ordering::Relaxed), 0);
        assert_eq!(state.stats.stats.failed_requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_runtime_route_no_tools_returns_ouyi() {
        let req = make_messages_request(vec![]);
        let route = runtime_route(&req, None);
        assert!(matches!(route, RuntimeRoute::OriginalOuyi));
    }

    #[test]
    fn test_runtime_route_tools_without_config_uses_compiler() {
        let req = make_request_with_tools();
        let route = runtime_route(&req, None);
        assert!(matches!(route, RuntimeRoute::Compiler));
    }

    #[test]
    fn test_runtime_route_tools_anthropic_provider_uses_compiler() {
        let req = make_request_with_tools();
        let config = make_anthropic_config();
        let route = runtime_route(&req, Some(&config));
        assert!(matches!(route, RuntimeRoute::Compiler));
    }

    #[test]
    fn test_runtime_route_tools_compiler_openai() {
        let req = make_request_with_tools();
        let config = make_compiler_config();
        let route = runtime_route(&req, Some(&config));
        assert!(matches!(route, RuntimeRoute::Compiler));
    }

    #[test]
    fn test_runtime_route_legacy_native_openai_still_uses_compiler() {
        let req = make_request_with_tools();
        let config = crate::config::ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Native,
            provider: crate::config::ToolRuntimeProvider::OpenaiCompatible,
            base_url: None,
            api_key: None,
            api_key_env: None,
            model: None,
            timeout_secs: 120,
        };
        let route = runtime_route(&req, Some(&config));
        assert!(matches!(route, RuntimeRoute::Compiler));
    }

    #[test]
    fn test_runtime_route_compiler_anthropic_uses_compiler() {
        let req = make_request_with_tools();
        let config = crate::config::ToolRuntimeConfig {
            enabled: true,
            mode: crate::config::ToolRuntimeMode::Compiler,
            provider: crate::config::ToolRuntimeProvider::Anthropic,
            base_url: None,
            api_key: None,
            api_key_env: None,
            model: None,
            timeout_secs: 120,
        };
        let route = runtime_route(&req, Some(&config));
        assert!(matches!(route, RuntimeRoute::Compiler));
    }

    // ModelRouting 测试
    #[test]
    fn test_resolve_model_routing_preserves_client_model_and_maps_to_ouyi() {
        let mapper = ModelMapper::new();
        mapper.load_fallback();

        let routing = resolve_model_routing("claude-sonnet-4-5", &mapper);

        assert_eq!(routing.client_model, "claude-sonnet-4-5");
        assert_eq!(routing.effective_model, "claude-sonnet-4-5");
        assert_eq!(
            routing.ouyi_model,
            "claude_chat::claude-sonnet-4-5-20250929-vip"
        );
    }

    #[test]
    fn test_resolve_model_routing_does_not_rewrite_unknown_models_to_sonnet() {
        let mapper = ModelMapper::new();
        mapper.load_fallback();

        let routing = resolve_model_routing("unknown-model-xyz", &mapper);

        assert_eq!(routing.client_model, "unknown-model-xyz");
        assert_eq!(routing.effective_model, "unknown-model-xyz");
        assert_eq!(routing.ouyi_model, "unknown-model-xyz");
    }

    fn write_tool() -> AnthropicTool {
        AnthropicTool {
            name: "Write".to_string(),
            description: "Write a file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    async fn ouyi_payload_write_handler() -> axum::response::Response {
        ouyi_sse_response(concat!(
            "准备将报告写入文件。\n",
            "<ouyi_payload id=\"p1\">\n",
            "# 报告\n",
            "\"quote\"\n",
            "</ouyi_payload>\n",
            "将 payload p1 写入 docs/report.md。"
        ))
    }

    async fn tool_compiler_write_payload_handler(
        State(counter): State<Arc<AtomicUsize>>,
    ) -> axum::Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_from_tool_model",
                        "type": "function",
                        "function": {
                            "name": "Write",
                            "arguments": "{\"file_path\":\"docs/report.md\",\"content_ref\":\"p1\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        }))
    }

    async fn tool_compiler_bad_arguments_handler(
        State(counter): State<Arc<AtomicUsize>>,
    ) -> axum::Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_bad",
                        "type": "function",
                        "function": {
                            "name": "Write",
                            "arguments": "{\"file_path\":\"docs/report.md\",\"content\":\""
                        }
                    }]
                }
            }]
        }))
    }

    #[tokio::test]
    async fn test_payload_write_tool_use_preserves_ouyi_content() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route("/api/chat/completions", post(ouyi_payload_write_handler)),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route(
                    "/chat/completions",
                    post(tool_compiler_write_payload_handler),
                )
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![write_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state, req, routing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(tool_requests.load(Ordering::SeqCst), 1);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stop_reason"], "tool_use");
        assert_eq!(value["content"][0]["type"], "tool_use");
        assert_eq!(value["content"][0]["name"], "Write");
        assert_eq!(value["content"][0]["input"]["content"], "# 报告\n\"quote\"");
        assert_eq!(value["content"][0]["input"]["file_path"], "docs/report.md");
    }

    #[tokio::test]
    async fn test_tool_compiler_bad_arguments_does_not_return_400() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route("/api/chat/completions", post(ouyi_payload_write_handler)),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route(
                    "/chat/completions",
                    post(tool_compiler_bad_arguments_handler),
                )
                .with_state(tool_requests.clone()),
        )
        .await;

        let state = make_test_state(test_config(ouyi_base_url, tool_base_url));
        let mut req = make_request_with_tools();
        req.stream = Some(false);
        req.tools = Some(vec![write_tool()]);
        let routing = resolve_model_routing(&req.model, &state.model_mapper);

        let response = handle_compiler_tool_runtime_message(state, req, routing)
            .await
            .unwrap();
        assert_ne!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}
