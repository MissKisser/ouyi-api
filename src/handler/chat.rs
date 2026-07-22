use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use futures::stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::adapter::openai_tool_response::{
    build_openai_text_response, build_openai_tool_call_response, build_text_content_chunk,
    build_text_finish_chunk, build_text_role_chunk, build_tool_call_delta_chunk,
    build_tool_call_finish_chunk, build_tool_call_role_chunk,
};
use crate::adapter::request::convert_request;
use crate::adapter::response::{SseCollector, StreamConverter};
use crate::client::http::OuyiHttpClient;
use crate::client::session::{GuardedStream, spawn_session_cleanup};
use crate::client::sse::{SseLineBuffer, ouyi_application_error, parse_sse_line};
use crate::error::AppError;
use crate::model::openai::ChatCompletionRequest;
use crate::platform::OuyiPlatform;
use crate::tool_runtime::broker::{self, LongTaskResult, classify_long_task_result};
use crate::tool_runtime::openai_compiler::CompiledToolCall;
use crate::tool_runtime::protocol::openai_tools_to_specs;

pub async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();

    // 请求大小检查
    let messages_size: usize = req
        .messages
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len())
        .sum();
    if messages_size > config.limits.max_messages_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "消息总大小 {} 超过限制 {}",
            messages_size, config.limits.max_messages_bytes
        )));
    }

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    // 长时任务轮询检查（所有平台共用）
    let has_tools = crate::tool_runtime::protocol::openai_tools_enabled(&req);
    if has_tools {
        if let Some(response) =
            maybe_build_openai_long_task_poll_response(&state, &req, &req.model, start_time).await?
        {
            return Ok(response);
        }
    }

    // 按平台分派
    let platform = *state.platform.read().await;
    match platform {
        OuyiPlatform::Ouyi7 => chat_completions_v7(state, req, start_time).await,
        OuyiPlatform::Ouyi8 => chat_completions_v8(state, req, start_time).await,
    }
}

fn record_chat_error(
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

/// 8.0 平台聊天处理
async fn chat_completions_v8(
    state: Arc<AppState>,
    req: ChatCompletionRequest,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    let platform_str = state.platform.read().await.config_key();

    let model_full = state.model_mapper.to_ouyi(&req.model);
    let should_thinking = state.model_mapper.should_thinking(&req.model);

    let has_tools = crate::tool_runtime::protocol::openai_tools_enabled(&req);

    // 请求转换
    let ouyi_req = match convert_request(&req, &model_full, should_thinking) {
        Ok(req) => req,
        Err(e) => {
            let err = AppError::Internal(e);
            record_chat_error(&state, start_time, &model_full, &err, &platform_str);
            return Err(err);
        }
    };

    let jwt_token = match state.account_store.get_next_token().await {
        Some(token) => token,
        None => config.ouyi.jwt_token.clone(),
    };

    if jwt_token.is_empty() {
        let err = AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string());
        record_chat_error(&state, start_time, &model_full, &err, &platform_str);
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
                &platform_str,
            );
            return Err(AppError::Upstream(msg));
        }
    };
    let session_id = session_data.id;

    state.active_sessions.insert(session_id, ());

    let final_text = if let Some(sys_prompt) = &ouyi_req.system_prompt {
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
            let msg = format!("聊天请求失败: {}", e);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                &platform_str,
            );
            state.active_sessions.remove(&session_id);
            spawn_session_cleanup(client, session_id);
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;

    if has_tools {
        let short_model = req.model.clone();
        handle_openai_tools_response(
            state.clone(),
            req,
            model_full,
            short_model,
            client,
            session_id,
            resp,
            start_time,
        )
        .await
    } else if req.stream {
        handle_stream_response(
            resp,
            model_full,
            session_id,
            client,
            state.clone(),
            start_time,
        )
        .await
    } else {
        handle_non_stream_response(resp, model_full, session_id, client, state.clone(), elapsed)
            .await
    }
}

/// 7.0 平台聊天处理
async fn chat_completions_v7(
    state: Arc<AppState>,
    req: ChatCompletionRequest,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    let platform_str = state.platform.read().await.config_key();

    let model_full = state.model_mapper.to_ouyi(&req.model);
    let v7_config = match config.ouyi_v7.as_ref() {
        Some(config) => config,
        None => {
            let err = AppError::Config("未配置 7.0 平台".to_string());
            record_chat_error(&state, start_time, &model_full, &err, &platform_str);
            return Err(err);
        }
    };
    let max_history = v7_config.max_history_messages;

    let has_tools = crate::tool_runtime::protocol::openai_tools_enabled(&req);

    // 请求转换
    let ouyi_input = match crate::adapter::ouyi_v7_request::from_openai_chat(
        &req,
        model_full.clone(),
        max_history,
    ) {
        Ok(input) => input,
        Err(e) => {
            record_chat_error(&state, start_time, &model_full, &e, &platform_str);
            return Err(e);
        }
    };

    let client = {
        let ouyi_client = state.ouyi_client.read().await;
        match &*ouyi_client {
            crate::client::OuyiClient::V7(client) => client.clone(),
            crate::client::OuyiClient::V8(_) => {
                let err = AppError::Internal("当前运行客户端不是 7.0".to_string());
                record_chat_error(&state, start_time, &model_full, &err, &platform_str);
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
                &platform_str,
            );
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;
    let short_model = req.model.clone();

    if has_tools {
        handle_openai_tools_plain_output(
            state,
            req,
            model_full,
            short_model,
            text,
            0,
            0,
            start_time,
            platform_str,
        )
        .await
    } else if req.stream {
        handle_v7_stream_response(&text, &short_model, model_full, state, start_time).await
    } else {
        handle_v7_non_stream_response(&text, &short_model, model_full, state, elapsed).await
    }
}

async fn handle_v7_stream_response(
    text: &str,
    model: &str,
    model_full: String,
    state: Arc<AppState>,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    use crate::adapter::ouyi_v7_response::build_openai_text_stream_from_plain;

    let config = state.config.read().await;
    let chunk_chars = config
        .ouyi_v7
        .as_ref()
        .map(|v| v.simulated_stream_chunk_chars)
        .unwrap_or(80);

    let task_id = format!("chatcmpl-{}", uuid_short());
    let response = match build_openai_text_stream_from_plain(model, &task_id, text, chunk_chars) {
        Ok(response) => response,
        Err(e) => {
            let platform_key = state.platform.read().await.config_key();
            record_chat_error(&state, start_time, &model_full, &e, &platform_key);
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

async fn handle_v7_non_stream_response(
    text: &str,
    model: &str,
    model_full: String,
    state: Arc<AppState>,
    elapsed: u64,
) -> Result<axum::response::Response, AppError> {
    use crate::adapter::ouyi_v7_response::build_openai_text_response_from_plain;

    let task_id = format!("chatcmpl-{}", uuid_short());
    let response = build_openai_text_response_from_plain(model, &task_id, text);

    let platform_key = state.platform.read().await.config_key();
    state
        .stats
        .record_success(elapsed, 0, 0, &model_full, &platform_key);

    Ok(Json(response).into_response())
}

async fn handle_stream_response(
    resp: reqwest::Response,
    model: String,
    session_id: i64,
    client: OuyiHttpClient,
    state: Arc<AppState>,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let task_id = format!("chatcmpl-{}", uuid_short());

    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|result| result.map_err(std::io::Error::other)),
        session_id,
        client,
    );

    let model_for_task = model.clone();
    let task_id_for_task = task_id.clone();
    let model_for_stats = model.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, axum::Error>>(32);

    tokio::spawn(async move {
        let mut converter = StreamConverter::new(model_for_task, task_id_for_task);
        let mut sse_buffer = SseLineBuffer::new();

        let mut partial = Vec::new();
        let mut stream_error: Option<String> = None;
        let mut application_error: Option<String> = None;
        let mut downstream_closed = false;
        futures::pin_mut!(guarded);

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
                let Some(event) = parse_sse_line(&line) else {
                    continue;
                };
                if let Some(msg) = ouyi_application_error(&event) {
                    application_error = Some(msg);
                    break 'read_loop;
                }
                if let Some(chunks) = converter.convert_event(&event) {
                    for chunk in chunks {
                        let json = match serde_json::to_string(&chunk) {
                            Ok(j) => j,
                            Err(_) => continue,
                        };
                        let sse_event = Event::default().data(json);
                        if tx.send(Ok(sse_event)).await.is_err() {
                            downstream_closed = true;
                            break 'read_loop;
                        }
                    }
                }
            }
        }

        if downstream_closed {
            state.active_sessions.remove(&session_id);
            return;
        }

        if let Some(msg) = application_error {
            let platform_key = state.platform.read().await.config_key();
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_for_stats,
                &msg,
                platform_key,
            );
            state.active_sessions.remove(&session_id);
            let data = serde_json::json!({
                "error": {
                    "message": msg,
                    "type": "upstream_error"
                }
            });
            let _ = tx
                .send(Ok(Event::default().event("error").data(data.to_string())))
                .await;
            return;
        }

        if let Some(msg) = stream_error {
            let platform_key = state.platform.read().await.config_key();
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_for_stats,
                &msg,
                &platform_key,
            );
            state.active_sessions.remove(&session_id);
            let data = serde_json::json!({
                "error": {
                    "message": msg,
                    "type": "stream_error"
                }
            });
            let _ = tx
                .send(Ok(Event::default().event("error").data(data.to_string())))
                .await;
            return;
        }

        // 处理剩余缓冲
        if let Some(remaining) = sse_buffer.drain_remaining()
            && let Some(event) = parse_sse_line(&remaining)
        {
            if let Some(msg) = ouyi_application_error(&event) {
                let platform_key = state.platform.read().await.config_key();
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    &model_for_stats,
                    &msg,
                    platform_key,
                );
                state.active_sessions.remove(&session_id);
                let data = serde_json::json!({
                    "error": {
                        "message": msg,
                        "type": "upstream_error"
                    }
                });
                let _ = tx
                    .send(Ok(Event::default().event("error").data(data.to_string())))
                    .await;
                return;
            }
            if let Some(chunks) = converter.convert_event(&event) {
                for chunk in chunks {
                    let json = match serde_json::to_string(&chunk) {
                        Ok(j) => j,
                        Err(_) => continue,
                    };
                    let sse_event = Event::default().data(json);
                    let _ = tx.send(Ok(sse_event)).await;
                }
            }
        }

        let (prompt, completion) = converter.tokens();
        let stream_elapsed = start_time.elapsed().as_millis() as u64;
        let platform_key = state.platform.read().await.config_key();
        state.stats.record_success(
            stream_elapsed,
            prompt,
            completion,
            &model_for_stats,
            &platform_key,
        );
        state.active_sessions.remove(&session_id);

        // 发送 [DONE]
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    let stream = ReceiverStream::new(rx);
    let sse = Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    );

    Ok(sse.into_response())
}

async fn handle_non_stream_response(
    resp: reqwest::Response,
    model: String,
    session_id: i64,
    client: OuyiHttpClient,
    state: Arc<AppState>,
    elapsed: u64,
) -> Result<axum::response::Response, AppError> {
    let task_id = format!("chatcmpl-{}", uuid_short());
    let mut collector = SseCollector::new(model.clone(), task_id);
    let mut sse_buffer = SseLineBuffer::new();

    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|result| result.map_err(std::io::Error::other)),
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
            let msg = format!("响应超过大小限制: {} > {}", total_bytes, max_bytes);
            tracing::warn!(total_bytes, max_bytes, "响应超过大小限制");
            stream_error = Some(msg);
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
        let platform_key = state.platform.read().await.config_key();
        state
            .stats
            .record_error(elapsed, &model, &msg, platform_key);
        return Err(AppError::Upstream(msg));
    }

    if let Some(msg) = stream_error {
        guarded.as_mut().skip_cleanup();
        spawn_session_cleanup(client, session_id);
        state.active_sessions.remove(&session_id);
        let platform_key = state.platform.read().await.config_key();
        state
            .stats
            .record_error(elapsed, &model, &msg, &platform_key);
        return Err(AppError::SseStream(msg));
    }

    let (prompt, completion) = collector.tokens();
    let response = collector.build_response();

    guarded.as_mut().skip_cleanup();
    spawn_session_cleanup(client, session_id);
    state.active_sessions.remove(&session_id);
    let platform_key = state.platform.read().await.config_key();
    state
        .stats
        .record_success(elapsed, prompt, completion, &model, &platform_key);

    Ok(Json(response).into_response())
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", ts % 0xFFFFFF)
}

/// 检查是否有长时任务需要轮询，如果有则生成轮询响应
async fn maybe_build_openai_long_task_poll_response(
    state: &AppState,
    req: &ChatCompletionRequest,
    model: &str,
    start_time: std::time::Instant,
) -> Result<Option<axum::response::Response>, AppError> {
    let platform_key = state.platform.read().await.config_key();
    let Some((tool_call_id, content)) = crate::tool_runtime::latest_openai_tool_result(req) else {
        return Ok(None);
    };

    let Some(task) = state
        .tool_ledger
        .long_task_for_tool_use(&tool_call_id, model)
    else {
        return Ok(None);
    };

    match classify_long_task_result(&content, &task.done_marker) {
        LongTaskResult::Done => {
            state.tool_ledger.complete_long_task(&task.task_id);
            Ok(None)
        }
        LongTaskResult::Pending => {
            let updated = match state
                .tool_ledger
                .increment_long_task_poll_by_task_id(&task.task_id)
            {
                Some(updated) => updated,
                None => {
                    let err = AppError::BadRequest("长时任务状态不存在".to_string());
                    record_chat_error(state, start_time, model, &err, &platform_key);
                    return Err(err);
                }
            };
            if updated.polls > updated.max_polls {
                state.tool_ledger.complete_long_task(&task.task_id);
                let msg = format!("长时任务轮询超限: {}", updated.file_path);
                let platform_key = state.platform.read().await.config_key();
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    model,
                    &msg,
                    &platform_key,
                );
                return Err(AppError::Upstream(msg));
            }

            let tool_specs = req
                .tools
                .as_ref()
                .map(|tools| openai_tools_to_specs(tools.as_slice()))
                .unwrap_or_default();

            let poll_call = match broker::build_poll_call(
                &broker::LongTaskPlan {
                    file_path: updated.file_path,
                    done_marker: updated.done_marker,
                    interval_secs: 5,
                    max_polls: updated.max_polls,
                },
                &tool_specs,
            ) {
                Ok(call) => call,
                Err(e) => {
                    state.stats.record_error(
                        start_time.elapsed().as_millis() as u64,
                        model,
                        &e,
                        &platform_key,
                    );
                    return Err(AppError::BadRequest(e));
                }
            };

            let poll_key = format!("{}:{}", model, poll_call.id);
            if let Err(e) = state
                .tool_ledger
                .record_tool_use(&poll_key, &poll_call.id, "Bash")
            {
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    model,
                    &e.to_string(),
                    &platform_key,
                );
                return Err(e);
            }
            if let Err(e) = state
                .tool_ledger
                .link_poll_to_long_task(&poll_key, &task.task_id)
            {
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    model,
                    &e.to_string(),
                    &platform_key,
                );
                return Err(e);
            }

            let calls = vec![CompiledToolCall {
                id: poll_call.id,
                name: poll_call.name,
                input: poll_call.input,
            }];

            let task_id = format!("chatcmpl-{}", uuid_short());
            let response = if req.stream {
                match build_openai_tool_call_stream_response(model, &task_id, calls) {
                    Ok(response) => response,
                    Err(e) => {
                        record_chat_error(state, start_time, model, &e, &platform_key);
                        return Err(e);
                    }
                }
            } else {
                Json(build_openai_tool_call_response(
                    model, &task_id, calls, 0, 0,
                ))
                .into_response()
            };
            state.stats.record_success(
                start_time.elapsed().as_millis() as u64,
                0,
                0,
                model,
                &platform_key,
            );
            Ok(Some(response))
        }
        LongTaskResult::Failed(msg) => {
            state.tool_ledger.complete_long_task(&task.task_id);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                model,
                &msg,
                &platform_key,
            );
            Err(AppError::Upstream(msg))
        }
    }
}

/// 处理带有工具的 OpenAI 请求响应
async fn handle_openai_tools_response(
    state: Arc<AppState>,
    req: ChatCompletionRequest,
    model_full: String,
    short_model: String,
    client: OuyiHttpClient,
    session_id: i64,
    resp: reqwest::Response,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let task_id = format!("chatcmpl-{}", uuid_short());
    let mut collector = SseCollector::new(model_full.clone(), task_id.clone());
    let mut sse_buffer = SseLineBuffer::new();

    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|result| result.map_err(std::io::Error::other)),
        session_id,
        client.clone(),
    );

    futures::pin_mut!(guarded);

    let max_bytes = state.config.read().await.limits.max_response_bytes;
    let mut total_bytes: usize = 0;
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
            let msg = format!("响应超过大小限制: {} > {}", total_bytes, max_bytes);
            tracing::warn!(total_bytes, max_bytes, "响应超过大小限制");
            stream_error = Some(msg);
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
        let platform_key = state.platform.read().await.config_key();
        state.stats.record_error(
            start_time.elapsed().as_millis() as u64,
            &model_full,
            &msg,
            platform_key,
        );
        return Err(AppError::Upstream(msg));
    }

    if let Some(msg) = stream_error {
        guarded.as_mut().skip_cleanup();
        spawn_session_cleanup(client, session_id);
        state.active_sessions.remove(&session_id);
        let platform_key = state.platform.read().await.config_key();
        state.stats.record_error(
            start_time.elapsed().as_millis() as u64,
            &model_full,
            &msg,
            &platform_key,
        );
        return Err(AppError::SseStream(msg));
    }

    let (prompt_tokens, completion_tokens) = collector.tokens();
    let ouyi_output = collector.text_content();

    guarded.as_mut().skip_cleanup();
    spawn_session_cleanup(client, session_id);
    state.active_sessions.remove(&session_id);

    let platform_key = state.platform.read().await.config_key();

    handle_openai_tools_plain_output(
        state,
        req,
        model_full,
        short_model,
        ouyi_output,
        prompt_tokens,
        completion_tokens,
        start_time,
        platform_key,
    )
    .await
}

/// 处理纯文本输出后的工具决策响应
async fn handle_openai_tools_plain_output(
    state: Arc<AppState>,
    req: ChatCompletionRequest,
    _model_full: String,
    short_model: String,
    ouyi_output: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    start_time: std::time::Instant,
    platform_key: &'static str,
) -> Result<axum::response::Response, AppError> {
    let task_id = format!("chatcmpl-{}", uuid_short());
    let elapsed = start_time.elapsed().as_millis() as u64;

    let pipeline_out = crate::tool_runtime::pipeline::run_openai_tool_pipeline(
        &state,
        &req,
        ouyi_output,
        platform_key,
        start_time,
    )
    .await?;

    match pipeline_out {
        crate::tool_runtime::pipeline::PipelineOutput::ToolCalls { calls, meta: _ } => {
            state.stats.record_success(
                elapsed,
                prompt_tokens,
                completion_tokens,
                &short_model,
                platform_key,
            );
            if req.stream {
                build_openai_tool_call_stream_response(&short_model, &task_id, calls)
            } else {
                let response = build_openai_tool_call_response(
                    &short_model,
                    &task_id,
                    calls,
                    prompt_tokens,
                    completion_tokens,
                );
                Ok(Json(response).into_response())
            }
        }
        crate::tool_runtime::pipeline::PipelineOutput::FinalAnswer { text, meta: _ } => {
            if req.stream {
                build_openai_text_stream_response(&short_model, &task_id, &text)
            } else {
                let response = build_openai_text_response(
                    &short_model,
                    &task_id,
                    &text,
                    prompt_tokens,
                    completion_tokens,
                );
                Ok(Json(response).into_response())
            }
        }
    }
}

fn sse_response_from_chunks(
    chunks: Vec<crate::model::openai::ChatCompletionChunk>,
) -> Result<axum::response::Response, AppError> {
    let mut events: Vec<Result<Event, axum::Error>> = Vec::with_capacity(chunks.len() + 1);
    for chunk in chunks {
        let json = serde_json::to_string(&chunk).map_err(|e| AppError::Internal(e.to_string()))?;
        events.push(Ok(Event::default().data(json)));
    }
    events.push(Ok(Event::default().data("[DONE]")));
    Ok(Sse::new(tokio_stream::iter(events)).into_response())
}

fn build_openai_tool_call_stream_response(
    model: &str,
    task_id: &str,
    calls: Vec<CompiledToolCall>,
) -> Result<axum::response::Response, AppError> {
    let mut chunks = Vec::with_capacity(calls.len() + 2);
    chunks.push(build_tool_call_role_chunk(model, task_id));
    for (idx, call) in calls.iter().enumerate() {
        chunks.push(build_tool_call_delta_chunk(
            model, task_id, idx as u32, call,
        ));
    }
    chunks.push(build_tool_call_finish_chunk(model, task_id));
    sse_response_from_chunks(chunks)
}

fn build_openai_text_stream_response(
    model: &str,
    task_id: &str,
    content: &str,
) -> Result<axum::response::Response, AppError> {
    let mut chunks = vec![build_text_role_chunk(model, task_id)];
    if !content.is_empty() {
        chunks.push(build_text_content_chunk(model, task_id, content));
    }
    chunks.push(build_text_finish_chunk(model, task_id));
    sse_response_from_chunks(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::AccountStore;
    use crate::admin::auth::AdminStore;
    use crate::admin::stats::StatsCollector;
    use crate::config::{
        AdminConfig, AppConfig, LimitsConfig, OuyiConfig, ServerConfig, ToolCallConfig,
        ToolRuntimeConfig, ToolRuntimeMode, ToolRuntimeProvider,
    };
    use crate::model::mapper::ModelMapper;
    use crate::model::openai::{ChatMessage, ToolDef, ToolFunction};
    use axum::Router;
    use axum::body::Body;
    use axum::http::{StatusCode, header};
    use axum::routing::{delete, post};
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            tool_runtime: None,
            openai_tool_runtime: Some(ToolRuntimeConfig {
                enabled: true,
                mode: ToolRuntimeMode::Compiler,
                provider: ToolRuntimeProvider::OpenaiCompatible,
                base_url: Some(tool_base_url),
                api_key: Some("sk-test".to_string()),
                api_key_env: None,
                model: Some("openai-tool-model".to_string()),
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

    async fn ouyi_session_handler() -> axum::Json<serde_json::Value> {
        axum::Json(json!({
            "code": 0,
            "msg": "ok",
            "data": {
                "id": 42,
                "model": "gpt-test",
                "contextCount": 0,
                "temperature": 0.0
            }
        }))
    }

    async fn ouyi_delete_session_handler() -> StatusCode {
        StatusCode::NO_CONTENT
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
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }

    async fn ouyi_plain_tool_need_handler() -> axum::response::Response {
        ouyi_sse_response("需要查看当前目录。")
    }

    async fn ouyi_application_error_handler() -> axum::response::Response {
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"id\":\"quota\",\"type\":\"\",\"data\":null,\"err\":\"您的套餐余量不足，请购买后重试\",\"code\":1}\n\n",
                "data: [DONE]\n\n"
            )))
            .unwrap()
    }

    async fn tool_compiler_handler(
        axum::extract::State(counter): axum::extract::State<Arc<AtomicUsize>>,
    ) -> axum::Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_from_openai_tool_model",
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

    fn bash_tool_def() -> ToolDef {
        ToolDef {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "Bash".to_string(),
                description: Some("Run a shell command".to_string()),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    },
                    "required": ["command"]
                })),
            },
        }
    }

    fn openai_request(stream: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("请查看当前目录".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream,
            temperature: None,
            max_tokens: None,
            tools: Some(vec![bash_tool_def()]),
            tool_choice: Some(json!("auto")),
            parallel_tool_calls: None,
        }
    }

    async fn setup_tool_runtime_state(tool_requests: Arc<AtomicUsize>) -> Arc<AppState> {
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/session/{id}",
                    delete(ouyi_delete_session_handler),
                )
                .route("/api/chat/completions", post(ouyi_plain_tool_need_handler)),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_handler))
                .with_state(tool_requests),
        )
        .await;
        make_test_state(test_config(ouyi_base_url, tool_base_url))
    }

    async fn setup_application_error_state(tool_requests: Arc<AtomicUsize>) -> Arc<AppState> {
        let ouyi_base_url = spawn_mock_router(
            Router::new()
                .route("/api/chat/session", post(ouyi_session_handler))
                .route(
                    "/api/chat/session/{id}",
                    delete(ouyi_delete_session_handler),
                )
                .route(
                    "/api/chat/completions",
                    post(ouyi_application_error_handler),
                ),
        )
        .await;
        let tool_base_url = spawn_mock_router(
            Router::new()
                .route("/chat/completions", post(tool_compiler_handler))
                .with_state(tool_requests),
        )
        .await;
        make_test_state(test_config(ouyi_base_url, tool_base_url))
    }

    #[tokio::test]
    async fn test_openai_non_stream_rejects_ouyi_application_error() {
        let state = setup_application_error_state(Arc::new(AtomicUsize::new(0))).await;
        let mut req = openai_request(false);
        req.tools = None;
        req.tool_choice = None;

        let err = chat_completions(State(state), Json(req)).await.unwrap_err();

        assert!(
            matches!(err, AppError::Upstream(message) if message.contains("您的套餐余量不足，请购买后重试"))
        );
    }

    #[tokio::test]
    async fn test_openai_stream_emits_error_without_done_for_ouyi_application_error() {
        let state = setup_application_error_state(Arc::new(AtomicUsize::new(0))).await;
        let mut req = openai_request(true);
        req.tools = None;
        req.tool_choice = None;

        let response = chat_completions(State(state), Json(req)).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_text = String::from_utf8(body.to_vec()).unwrap();

        assert!(body_text.contains("event: error"));
        assert!(body_text.contains("您的套餐余量不足，请购买后重试"));
        assert!(!body_text.contains("[DONE]"));
    }

    #[tokio::test]
    async fn test_openai_tools_reject_ouyi_application_error_before_compiler() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let state = setup_application_error_state(tool_requests.clone()).await;

        let err = chat_completions(State(state), Json(openai_request(false)))
            .await
            .unwrap_err();

        assert!(
            matches!(err, AppError::Upstream(message) if message.contains("您的套餐余量不足，请购买后重试"))
        );
        assert_eq!(tool_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_chat_missing_jwt_records_error_and_clears_active_request() {
        let mut config = test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        );
        config.ouyi.jwt_token.clear();
        let state = make_test_state(config);
        let req = openai_request(false);

        let err = chat_completions(State(state.clone()), Json(req))
            .await
            .unwrap_err();

        assert!(matches!(err, AppError::BadRequest(_)));
        assert_eq!(state.stats.stats.active_requests.load(Ordering::Relaxed), 0);
        assert_eq!(state.stats.stats.failed_requests.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_openai_tools_invokes_openai_tool_runtime_when_main_output_needs_compiler() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let state = setup_tool_runtime_state(tool_requests.clone()).await;

        let response = chat_completions(State(state.clone()), Json(openai_request(false)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(tool_requests.load(Ordering::SeqCst), 1);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "Bash"
        );

        let logs = state.stats.get_tool_runtime_logs(10);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].endpoint, "openai");
        assert_eq!(logs[0].model, "openai-tool-model");
        assert_eq!(logs[0].prompt_tokens, 11);
        assert_eq!(logs[0].completion_tokens, 3);
    }

    #[tokio::test]
    async fn test_openai_tools_streams_tool_calls_when_request_stream_true() {
        let tool_requests = Arc::new(AtomicUsize::new(0));
        let state = setup_tool_runtime_state(tool_requests.clone()).await;

        let response = chat_completions(State(state), Json(openai_request(true)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(tool_requests.load(Ordering::SeqCst), 1);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(content_type.starts_with("text/event-stream"));

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_text.contains("\"tool_calls\""));
        assert!(body_text.contains("call_from_openai_tool_model"));
        assert!(body_text.contains("[DONE]"));
    }

    #[tokio::test]
    async fn test_openai_long_task_poll_records_success_and_increments_poll_count() {
        let state = make_test_state(test_config(
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        ));
        state
            .tool_ledger
            .record_tool_use("gpt-test:call_start", "call_start", "Bash")
            .unwrap();
        let long_task_id = state
            .tool_ledger
            .attach_long_task(
                "gpt-test:call_start",
                "longtask.txt".to_string(),
                "DONE_MARKER".to_string(),
                3,
            )
            .unwrap();

        let mut req = openai_request(false);
        req.messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some("PENDING".to_string()),
            tool_calls: None,
            tool_call_id: Some("call_start".to_string()),
            name: Some("Bash".to_string()),
        });

        let response = chat_completions(State(state.clone()), Json(req))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state
                .stats
                .stats
                .active_requests
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            state
                .stats
                .stats
                .successful_requests
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            state
                .tool_ledger
                .long_task_by_id(&long_task_id)
                .unwrap()
                .polls,
            1
        );
    }

    // ===== 7.0 平台测试 =====
    fn make_v7_test_state(ouyi_base_url: String) -> Arc<AppState> {
        use crate::client::ouyi_v7::Ouyi7HttpClient;

        let http_client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();
        let platform = OuyiPlatform::Ouyi7;
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
            config: tokio::sync::RwLock::new(AppConfig {
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
                tool_runtime: None,
                openai_tool_runtime: None,
                platform: None,
                ouyi_v7: Some(crate::config::Ouyi7Config {
                    base_url: ouyi_base_url.clone(),
                    chatapi_prefix: "/chatapi".to_string(),
                    jwt_token: "test-v7-jwt".to_string(),
                    default_model: "ouyi-chat".to_string(),
                    model_refresh_interval_secs: 3600,
                    max_history_messages: 20,
                    simulated_stream_chunk_chars: 80,
                }),
            }),
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

    fn make_v7_test_state_with_tool_runtime(ouyi_base_url: String) -> Arc<AppState> {
        use crate::client::ouyi_v7::Ouyi7HttpClient;

        let http_client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();
        let platform = OuyiPlatform::Ouyi7;
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
            config: tokio::sync::RwLock::new(AppConfig {
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
                tool_runtime: None,
                openai_tool_runtime: None,
                platform: None,
                ouyi_v7: Some(crate::config::Ouyi7Config {
                    base_url: ouyi_base_url.clone(),
                    chatapi_prefix: "/chatapi".to_string(),
                    jwt_token: "test-v7-jwt".to_string(),
                    default_model: "ouyi-chat".to_string(),
                    model_refresh_interval_secs: 3600,
                    max_history_messages: 20,
                    simulated_stream_chunk_chars: 80,
                }),
            }),
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

    #[tokio::test]
    async fn test_ouyi7_chat_completions_supports_tools() {
        let mock_router = ouyi7_mock_router_with_reply(concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        ))
        .await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state_with_tool_runtime(base_url);

        let req = ChatCompletionRequest {
            model: "ouyi-chat".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: Some(vec![bash_tool_def()]),
            tool_choice: None,
            parallel_tool_calls: None,
        };

        let response = chat_completions(State(state.clone()), Json(req))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "Bash"
        );
        assert_eq!(value["usage"]["prompt_tokens"], 0);
        assert_eq!(value["usage"]["completion_tokens"], 0);
    }

    #[tokio::test]
    async fn test_ouyi7_openai_tool_result_continues_to_final_text() {
        let mock_router = ouyi7_mock_router_with_reply("目录里有 Cargo.toml。").await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state_with_tool_runtime(base_url);

        let req = ChatCompletionRequest {
            model: "ouyi-chat".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: Some("查看当前目录".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![crate::model::openai::ToolCall {
                        id: "call_1".to_string(),
                        call_type: "function".to_string(),
                        function: crate::model::openai::ToolCallFunction {
                            name: "Bash".to_string(),
                            arguments: "{\"command\":\"pwd\"}".to_string(),
                        },
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some("D:\\document\\Projects\\ouyi-api".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: Some("Bash".to_string()),
                },
            ],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: Some(vec![bash_tool_def()]),
            tool_choice: None,
            parallel_tool_calls: None,
        };

        let response = chat_completions(State(state.clone()), Json(req))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
        assert!(
            value["choices"][0]["message"]["content"]
                .as_str()
                .unwrap()
                .contains("Cargo.toml")
        );
        assert_eq!(value["usage"]["prompt_tokens"], 0);
        assert_eq!(value["usage"]["completion_tokens"], 0);
    }

    // ===== 7.0 OpenAI Chat 流式/非流式测试 =====
    async fn ouyi7_mock_router_with_reply(reply: &'static str) -> Router {
        use axum::routing::post;

        async fn save_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": {
                    "id": 12345,
                    "title": "Chat"
                }
            }))
        }

        async fn message_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": [12345, 67890]
            }))
        }

        async fn message_response_handler(
            State(reply): State<&'static str>,
        ) -> axum::response::Response {
            axum::response::Response::builder()
                .status(200)
                .header("Content-Type", "text/plain")
                .body(reply.into())
                .unwrap()
        }

        async fn delete_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
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

    async fn ouyi7_mock_router() -> Router {
        use axum::routing::post;

        async fn save_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": {
                    "id": 12345,
                    "title": "Chat"
                }
            }))
        }

        async fn message_handler() -> axum::Json<serde_json::Value> {
            axum::Json(json!({
                "code": 200,
                "msg": "ok",
                "result": [12345, 67890]
            }))
        }

        async fn message_response_handler() -> axum::response::Response {
            axum::response::Response::builder()
                .status(200)
                .header("Content-Type", "text/plain")
                .body("这是7.0平台的模拟回复文本".into())
                .unwrap()
        }

        Router::new()
            .route("/chatapi/chat/save", post(save_handler))
            .route("/chatapi/chat/message", post(message_handler))
            .route("/chatapi/chat/message/{id}", post(message_response_handler))
    }

    #[tokio::test]
    async fn test_ouyi7_chat_completions_non_stream() {
        let mock_router = ouyi7_mock_router().await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state(base_url);

        let req = ChatCompletionRequest {
            model: "ouyi-chat".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("你好，请介绍一下自己".to_string()),
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

        let response = chat_completions(State(state.clone()), Json(req))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["choices"][0]["finish_reason"], "stop");
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        assert!(content.contains("7.0平台") || !content.is_empty());
        assert_eq!(value["usage"]["prompt_tokens"], 0);
        assert_eq!(value["usage"]["completion_tokens"], 0);
    }

    #[tokio::test]
    async fn test_ouyi7_chat_completions_stream() {
        let mock_router = ouyi7_mock_router().await;
        let base_url = spawn_mock_router(mock_router).await;
        let state = make_v7_test_state(base_url);

        let req = ChatCompletionRequest {
            model: "ouyi-chat".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("你好，请介绍一下自己".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: true,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        let response = chat_completions(State(state.clone()), Json(req))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(content_type.starts_with("text/event-stream"));

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_text.contains("data:"));
        assert!(body_text.contains("[DONE]"));
    }
}
