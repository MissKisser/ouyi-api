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
use crate::client::http::OuyiHttpClient;
use crate::client::session::{GuardedStream, spawn_session_cleanup};
use crate::client::sse::{SseLineBuffer, parse_sse_line};
use crate::error::AppError;
use crate::model::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use crate::tool_runtime;

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

/// 生成会话缓存 key：模型名 + 最后一个 tool_use_id
fn session_cache_key(model: &str, tool_use_id: &str) -> String {
    format!("{}:{}", model, tool_use_id)
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
                } = block
                {
                    let tool_name = tool_id_to_name
                        .get(tool_use_id.as_str())
                        .map(|s| s.as_str())
                        .unwrap_or(tool_use_id.as_str());
                    let display_content = if content.len() > 2000 {
                        format!(
                            "{}\n\n[...内容截断，共 {} 字符...]",
                            &content[..2000],
                            content.len()
                        )
                    } else {
                        content.clone()
                    };
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
        "请根据以上工具执行结果继续。如果需要再次调用工具，请直接输出工具调用标记。".to_string(),
    );
    parts.join("\n\n")
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
fn prune_stale_tool_sessions(state: &AppState) {
    let mut to_remove = Vec::new();
    for entry in state.tool_sessions.iter() {
        if entry.value().created_at.elapsed().as_secs() > 300 {
            to_remove.push(entry.key().clone());
        }
    }
    for key in to_remove {
        if let Some((_, cached)) = state.tool_sessions.remove(&key) {
            tracing::debug!(session_id = cached.session_id, "清理过期工具会话缓存");
            // 过期会话需要在上游删除，使用临时 client
            let jwt_token = state.config.ouyi.jwt_token.clone();
            if !jwt_token.is_empty() {
                let client = OuyiHttpClient::new(
                    state.http_client.clone(),
                    state.config.ouyi.base_url.clone(),
                    jwt_token,
                    state.config.ouyi.app_version.clone(),
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
    tracing::info!(
        model = %req.model,
        messages_count = req.messages.len(),
        stream = ?req.stream,
        has_tools = req.tools.is_some(),
        "Anthropic messages 请求"
    );

    if tool_runtime::should_use_tool_runtime(&req) {
        return handle_tool_runtime_message(state, req).await;
    }

    let model_full = state.model_mapper.to_ouyi(&req.model);
    tracing::info!(model = %req.model, full = %model_full, "模型映射结果");

    let short_model = req.model.clone();
    let should_thinking = state.model_mapper.should_thinking(&req.model)
        || matches!(
            req.thinking,
            Some(crate::model::anthropic::ThinkingConfig::Enabled { .. })
                | Some(crate::model::anthropic::ThinkingConfig::Adaptive)
        );

    prune_stale_tool_sessions(&state);

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    let ouyi_req = convert_anthropic_request(&req, &model_full, should_thinking)
        .map_err(|e| AppError::BadRequest(e))?;

    // 判断是否为工具回传请求，尝试复用缓存会话
    let tool_use_id = extract_last_tool_use_id(&req);
    let is_tool_continuation = has_tool_result(&req) && tool_use_id.is_some();

    let (session_id, client, cache_key) = if is_tool_continuation {
        let tid = tool_use_id.as_ref().unwrap();
        let key = session_cache_key(&model_full, tid);
        match state.tool_sessions.remove(&key) {
            Some((_, cached)) => {
                tracing::info!(session_id = cached.session_id, tool_use_id = %tid, "复用工具会话");
                // 复用缓存的 session_id，但创建新的 client
                let jwt_token = match state.account_store.get_next_token().await {
                    Some(token) => token,
                    None => state.config.ouyi.jwt_token.clone(),
                };
                if jwt_token.is_empty() {
                    return Err(AppError::BadRequest(
                        "未配置 JWT token 且无可用账号 token".to_string(),
                    ));
                }
                let client = OuyiHttpClient::new(
                    state.http_client.clone(),
                    state.config.ouyi.base_url.clone(),
                    jwt_token,
                    state.config.ouyi.app_version.clone(),
                );
                (cached.session_id, client, Some(key))
            }
            None => {
                tracing::warn!(tool_use_id = %tid, "工具会话缓存未命中，创建新会话");
                let jwt_token = match state.account_store.get_next_token().await {
                    Some(token) => token,
                    None => state.config.ouyi.jwt_token.clone(),
                };
                if jwt_token.is_empty() {
                    return Err(AppError::BadRequest(
                        "未配置 JWT token 且无可用账号 token".to_string(),
                    ));
                }
                let client = OuyiHttpClient::new(
                    state.http_client.clone(),
                    state.config.ouyi.base_url.clone(),
                    jwt_token,
                    state.config.ouyi.app_version.clone(),
                );
                let session_data = match client.create_session(&model_full).await {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = format!("创建会话失败: {}", e);
                        state.stats.record_error(
                            start_time.elapsed().as_millis() as u64,
                            &model_full,
                            &msg,
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
            None => state.config.ouyi.jwt_token.clone(),
        };
        if jwt_token.is_empty() {
            return Err(AppError::BadRequest(
                "未配置 JWT token 且无可用账号 token".to_string(),
            ));
        }
        let client = OuyiHttpClient::new(
            state.http_client.clone(),
            state.config.ouyi.base_url.clone(),
            jwt_token,
            state.config.ouyi.app_version.clone(),
        );
        let session_data = match client.create_session(&model_full).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("创建会话失败: {}", e);
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    &model_full,
                    &msg,
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
            state
                .stats
                .record_error(start_time.elapsed().as_millis() as u64, &model_full, &msg);
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;
    let stream = req.stream.unwrap_or(false);

    if stream {
        handle_anthropic_stream(
            resp,
            short_model,
            model_full,
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

async fn handle_tool_runtime_message(
    state: Arc<AppState>,
    req: MessagesRequest,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.tool_runtime.clone().ok_or_else(|| {
        AppError::Config(
            "请求包含 tools，但未配置 [tool_runtime]；Ouyi Claude 不具备原生工具能力".to_string(),
        )
    })?;

    if let Some(tool_result_id) = tool_runtime::latest_tool_result_id(&req) {
        if !tool_runtime::has_matching_tool_use(&req, &tool_result_id) {
            let key = tool_runtime::session_key_for_request(&req).ok_or_else(|| {
                AppError::BadRequest("无法为 tool_result 生成工具会话 key".to_string())
            })?;
            state
                .tool_ledger
                .complete_tool_result(&key, &tool_result_id)?;
        }
    } else if let (Some(tool_use_id), Some(tool_name)) = (
        tool_runtime::latest_tool_use_id(&req),
        tool_runtime::latest_tool_use_name(&req),
    ) {
        let key = tool_runtime::session_key_for_request(&req)
            .ok_or_else(|| AppError::BadRequest("无法为 tool_use 生成工具会话 key".to_string()))?;
        state
            .tool_ledger
            .record_tool_use(&key, &tool_use_id, &tool_name)?;
    }

    match config.provider {
        crate::config::ToolRuntimeProvider::Anthropic => {
            tool_runtime::native_anthropic::proxy_messages(state.http_client.clone(), config, req)
                .await
        }
        crate::config::ToolRuntimeProvider::OpenaiCompatible => Err(AppError::Config(
            "tool_runtime.provider=openai-compatible 尚未启用；请使用 provider=\"anthropic\""
                .to_string(),
        )),
    }
}

async fn handle_anthropic_stream(
    resp: reqwest::Response,
    model: String,
    model_full: String,
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
    let model_full_for_cache = model_full.clone();
    let task_id_for_task = task_id.clone();

    let state_for_stream = state.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(32);

    tracing::info!(session_id, "启动 Anthropic 流式响应");

    tokio::spawn(async move {
        let mut converter = AnthropicStreamConverter::new(model_for_task, task_id_for_task);
        let mut sse_buffer = SseLineBuffer::new();
        let mut partial = Vec::new();

        futures::pin_mut!(guarded);

        let start_evt = converter.start_event();
        tracing::info!(event_len = start_evt.len(), "发送 message_start");
        if tx.send(start_evt).await.is_err() {
            tracing::warn!("发送 message_start 失败，channel 已关闭");
            return;
        }

        let mut chunk_count = 0u64;
        while let Some(chunk_result) = guarded.next().await {
            let chunk = match chunk_result {
                Ok(bytes) => bytes,
                Err(_) => {
                    tracing::warn!("上游字节流读取错误");
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
                    let event_type =
                        crate::client::sse::OuyiEventType::from(event.event_type.as_str());
                    let content = event.data.as_ref().and_then(|d| d.as_str()).unwrap_or("");
                    let is_done = event_type == crate::client::sse::OuyiEventType::Done;

                    if event_type == crate::client::sse::OuyiEventType::StringChunk {
                        tracing::info!(content_len = content.len(), content_preview = %content.chars().take(200).collect::<String>(), "上游 string 事件");
                    } else {
                        tracing::info!(
                            event_type = event.event_type,
                            is_done,
                            content_len = content.len(),
                            "上游事件"
                        );
                    }

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
                            return;
                        }
                    }
                }
            }
            chunk_count += 1;
        }

        if !converter.done_processed {
            tracing::info!("上游流结束，发送 done 事件");
            for evt in converter.process_ouyi_event("", true) {
                let _ = tx.send(evt).await;
            }
        }

        tracing::info!(
            chunk_count,
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
        );

        if has_tool_use {
            // tool_use 响应：阻止 GuardedStream Drop 自动删除会话
            guarded.as_mut().skip_cleanup();

            // 缓存会话 ID 供下一轮 tool_result 复用
            let last_tool_id = converter.last_tool_use_id.clone();
            if let Some(tool_id) = last_tool_id {
                let key = session_cache_key(&model_full_for_cache, &tool_id);
                state_for_stream.tool_sessions.insert(
                    key,
                    crate::CachedToolSession {
                        session_id,
                        model: model.clone(),
                        created_at: std::time::Instant::now(),
                    },
                );
                tracing::info!(session_id, tool_use_id = %tool_id, "缓存工具会话等待 tool_result 回传");
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
    tracing::info!(session_id, "Anthropic SSE 流构建完成，返回响应");

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
    let max_bytes = state.config.limits.max_response_bytes;
    let mut partial = Vec::new();

    while let Some(chunk_result) = guarded.next().await {
        let chunk = match chunk_result {
            Ok(bytes) => bytes,
            Err(_) => break,
        };

        total_bytes += chunk.len();
        if total_bytes > max_bytes {
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
                collector.push_event(&event);
            }
        }
    }

    if let Some(remaining) = sse_buffer.drain_remaining()
        && let Some(event) = parse_sse_line(&remaining)
    {
        collector.push_event(&event);
    }

    let (prompt, completion) = collector.tokens();
    let response = collector.build_response();

    state
        .stats
        .record_success(elapsed, prompt, completion, &model);

    // 根据 stop_reason 决定缓存或清理会话
    let has_tool_use = response.stop_reason == "tool_use";
    if has_tool_use {
        // 从响应中提取最后一个 tool_use 的 id 作为缓存 key
        let last_tool_id = response.content.iter().rev().find_map(|b| {
            if let crate::model::anthropic::ResponseContentBlock::ToolUse { id, .. } = b {
                Some(id.clone())
            } else {
                None
            }
        });
        if let Some(tool_id) = last_tool_id {
            let key = session_cache_key(&model, &tool_id);
            state.tool_sessions.insert(
                key,
                crate::CachedToolSession {
                    session_id,
                    model: model.clone(),
                    created_at: std::time::Instant::now(),
                },
            );
            tracing::info!(session_id, tool_use_id = %tool_id, "缓存工具会话等待 tool_result 回传");
        }
        // 不删除会话，保留给下一轮
    } else {
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
    use crate::model::anthropic::{AnthropicContent, AnthropicMessage, ContentBlock};

    fn make_tool_result_msg(tool_use_id: &str, content: &str) -> AnthropicMessage {
        AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
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
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "result B".to_string(),
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
    fn test_format_tool_results_for_reuse_truncation() {
        let long_content = "x".repeat(3000);
        let tool_result = make_tool_result_msg("call_0", &long_content);
        let req = make_messages_request(vec![tool_result]);

        let result = format_tool_results_for_reuse(&req);
        assert!(result.contains("截断"));
        assert!(result.contains("3000"));
        assert!(result.len() < long_content.len() + 200); // 应远小于原始大小
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
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "last".to_string(),
                },
            ]),
        };
        let req = make_messages_request(vec![msg]);
        assert_eq!(extract_last_tool_use_id(&req), Some("call_1".to_string()));
    }
}
