use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use futures::stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::adapter::request::convert_request;
use crate::adapter::response::SseCollector;
use crate::client::http::OuyiHttpClient;
use crate::client::session::{GuardedStream, spawn_session_cleanup};
use crate::client::sse::{SseLineBuffer, parse_sse_line};
use crate::error::AppError;
use crate::model::openai::*;
use crate::platform::OuyiPlatform;

#[path = "responses_stream.rs"]
mod responses_stream;

#[path = "responses_non_stream.rs"]
mod responses_non_stream;

pub async fn responses(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResponsesRequest>,
) -> Result<axum::response::Response, AppError> {
    let chat_req = convert_responses_to_chat(&req)?;
    let is_stream = req.stream;

    let model_full = state.model_mapper.to_ouyi(&chat_req.model);

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    // 按平台分派
    let platform = *state.platform.read().await;
    match platform {
        OuyiPlatform::Ouyi7 => responses_v7(state, req, chat_req, model_full, start_time).await,
        OuyiPlatform::Ouyi8 => {
            responses_v8(state, req, chat_req, model_full, start_time, is_stream).await
        }
    }
}

fn record_responses_error(
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

/// 8.0 平台 Responses 处理
async fn responses_v8(
    state: Arc<AppState>,
    req: ResponsesRequest,
    chat_req: ChatCompletionRequest,
    model_full: String,
    start_time: std::time::Instant,
    is_stream: bool,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    let should_thinking = state.model_mapper.should_thinking(&chat_req.model);

    let has_tools = req.tools.as_ref().is_some_and(|tools| !tools.is_empty());
    if has_tools {
        return responses_v8_tools(state, req, chat_req, model_full, start_time, is_stream).await;
    }

    let ouyi_req = match convert_request(&chat_req, &model_full, should_thinking) {
        Ok(req) => req,
        Err(e) => {
            let err = AppError::Internal(e);
            record_responses_error(&state, start_time, &model_full, &err, "ouyi8");
            return Err(err);
        }
    };

    let jwt_token = match state.account_store.get_next_token().await {
        Some(token) => token,
        None => config.ouyi.jwt_token.clone(),
    };

    if jwt_token.is_empty() {
        let err = AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string());
        record_responses_error(&state, start_time, &model_full, &err, "ouyi8");
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
                "ouyi8",
            );
            state.active_sessions.remove(&session_id);
            spawn_session_cleanup(client, session_id);
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;

    let task_id = format!("resp_{}", uuid_short());
    let short_model = state
        .model_mapper
        .get_all_short_names()
        .iter()
        .find(|s| state.model_mapper.to_ouyi(s) == model_full)
        .cloned()
        .unwrap_or_else(|| req.model.clone());

    if is_stream {
        handle_responses_stream(
            resp,
            task_id,
            short_model,
            model_full,
            session_id,
            client,
            state,
            start_time,
        )
        .await
    } else {
        handle_responses_non_stream(
            resp,
            task_id,
            short_model,
            model_full,
            session_id,
            client,
            state,
            elapsed,
        )
        .await
    }
}

/// 8.0 平台带 tools 的 Responses 处理。
///
/// 流程：拿 token → 建 client → 建会话 → `create_completion` 拿流 → 把 SSE 字节
/// 收集成纯文本 → 交给 `run_openai_tool_pipeline` 决策（继续补全 or 直接工具调用）
/// → 根据 `is_stream` 返回 SSE 事件流或 JSON 响应。
async fn responses_v8_tools(
    state: Arc<AppState>,
    req: ResponsesRequest,
    chat_req: crate::model::openai::ChatCompletionRequest,
    model_full: String,
    start_time: std::time::Instant,
    is_stream: bool,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    let _platform_key = state.platform.read().await.config_key();

    let jwt_token = match state.account_store.get_next_token().await {
        Some(t) => t,
        None => config.ouyi.jwt_token.clone(),
    };
    if jwt_token.is_empty() {
        let err = AppError::BadRequest("未配置 JWT token 且无可用账号 token".into());
        record_responses_error(&state, start_time, &model_full, &err, "ouyi8");
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

    let should_thinking = state.model_mapper.should_thinking(&chat_req.model);
    let ouyi_req = match convert_request(&chat_req, &model_full, should_thinking) {
        Ok(r) => r,
        Err(e) => {
            let err = AppError::Internal(e);
            record_responses_error(&state, start_time, &model_full, &err, "ouyi8");
            return Err(err);
        }
    };

    let resp = match client
        .create_completion(&session_data.id, &ouyi_req)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("创建补全失败: {}", e);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                "ouyi8",
            );
            state.active_sessions.remove(&session_data.id);
            spawn_session_cleanup(client, session_data.id);
            return Err(AppError::Upstream(msg));
        }
    };

    state.active_sessions.insert(session_data.id, ());

    // 收集上游文本（SSE bytes → String）
    use crate::client::sse::{SseLineBuffer, parse_sse_line};
    use futures::StreamExt;

    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|r| r.map_err(std::io::Error::other)),
        session_data.id,
        client.clone(),
    );
    futures::pin_mut!(guarded);

    let mut upstream_text = String::new();
    let mut sse_buf = SseLineBuffer::new();
    let mut partial: Vec<u8> = Vec::new();
    let mut total_bytes = 0usize;
    let max_bytes = state.config.read().await.limits.max_response_bytes;

    while let Some(chunk) = guarded.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("SSE字节流读取错误: {}", e);
                state.stats.record_error(
                    start_time.elapsed().as_millis() as u64,
                    &model_full,
                    &msg,
                    "ouyi8",
                );
                guarded.as_mut().skip_cleanup();
                spawn_session_cleanup(client, session_data.id);
                state.active_sessions.remove(&session_data.id);
                return Err(AppError::SseStream(msg));
            }
        };
        total_bytes += bytes.len();
        if total_bytes > max_bytes {
            let msg = format!("响应超过大小限制: {} > {}", total_bytes, max_bytes);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                "ouyi8",
            );
            guarded.as_mut().skip_cleanup();
            spawn_session_cleanup(client, session_data.id);
            state.active_sessions.remove(&session_data.id);
            return Err(AppError::PayloadTooLarge(msg));
        }
        let mut v = std::mem::take(&mut partial);
        v.extend_from_slice(&bytes);
        match String::from_utf8(v) {
            Ok(s) => {
                for line in sse_buf.push_chunk(&s) {
                    if let Some(event) = parse_sse_line(&line)
                        && let Some(data) = &event.data
                        && let Some(text) = data.as_str()
                    {
                        upstream_text.push_str(text);
                    }
                }
            }
            Err(e) => {
                partial = e.into_bytes();
            }
        }
    }
    if let Some(remaining) = sse_buf.drain_remaining()
        && let Some(event) = parse_sse_line(&remaining)
        && let Some(data) = &event.data
        && let Some(text) = data.as_str()
    {
        upstream_text.push_str(text);
    }

    guarded.as_mut().skip_cleanup();
    spawn_session_cleanup(client, session_data.id);
    state.active_sessions.remove(&session_data.id);

    // pipeline 决策
    let pipeline_out = crate::tool_runtime::pipeline::run_openai_tool_pipeline(
        &state,
        &chat_req,
        upstream_text,
        "ouyi8",
        start_time,
    )
    .await;

    let short_model = chat_req.model.clone();
    let task_id = format!("resp_{}", crate::tool_runtime::uuid_short());

    if is_stream {
        let events = responses_stream::events_for_pipeline_output(
            &task_id, &short_model, &pipeline_out,
        );
        let sse_events: Vec<Result<axum::response::sse::Event, axum::Error>> =
            events.into_iter().map(Ok).collect();
        let sse = axum::response::sse::Sse::new(tokio_stream::iter(sse_events));
        Ok(sse.into_response())
    } else {
        let prompt_tokens = 0u64;
        let completion_tokens = 0u64;
        let resp_json = responses_non_stream::build_responses_response_from_pipeline(
            &task_id, &short_model, &pipeline_out, prompt_tokens, completion_tokens,
        );
        Ok(Json(resp_json).into_response())
    }
}

/// 7.0 平台 Responses 处理
async fn responses_v7(
    state: Arc<AppState>,
    req: ResponsesRequest,
    chat_req: ChatCompletionRequest,
    model_full: String,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    let platform_key = state.platform.read().await.config_key();

    let v7_config = match config.ouyi_v7.as_ref() {
        Some(config) => config,
        None => {
            let err = AppError::Config("未配置 7.0 平台".to_string());
            record_responses_error(&state, start_time, &model_full, &err, platform_key);
            return Err(err);
        }
    };

    let max_history = v7_config.max_history_messages;

    let has_tools = req.tools.as_ref().is_some_and(|tools| !tools.is_empty());
    if has_tools {
        return responses_v7_tools(state, req, chat_req, model_full, start_time).await;
    }

    let ouyi_input = match crate::adapter::ouyi_v7_request::from_openai_chat(
        &chat_req,
        model_full.clone(),
        max_history,
    ) {
        Ok(input) => input,
        Err(e) => {
            record_responses_error(&state, start_time, &model_full, &e, platform_key);
            return Err(e);
        }
    };

    let client = {
        let ouyi_client = state.ouyi_client.read().await;
        match &*ouyi_client {
            crate::client::OuyiClient::V7(client) => client.clone(),
            crate::client::OuyiClient::V8(_) => {
                let err = AppError::Internal("当前运行客户端不是 7.0".to_string());
                record_responses_error(&state, start_time, &model_full, &err, platform_key);
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
                platform_key,
            );
            return Err(AppError::Upstream(msg));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;
    let short_model = state
        .model_mapper
        .get_all_short_names()
        .iter()
        .find(|s| state.model_mapper.to_ouyi(s) == model_full)
        .cloned()
        .unwrap_or_else(|| req.model.clone());

    if req.stream {
        handle_v7_responses_stream(&text, &short_model, model_full, state, start_time).await
    } else {
        handle_v7_responses_non_stream(
            &text,
            &short_model,
            model_full,
            state,
            elapsed,
            platform_key,
        )
        .await
    }
}

/// 7.0 平台带 tools 的 Responses 处理。
async fn responses_v7_tools(
    state: Arc<AppState>,
    req: ResponsesRequest,
    chat_req: ChatCompletionRequest,
    model_full: String,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let config = state.config.read().await.clone();
    let v7_config = config
        .ouyi_v7
        .clone()
        .ok_or_else(|| AppError::Internal("未配置 ouyi_v7".to_string()))?;
    let platform_key = state.platform.read().await.config_key();

    let ouyi_input = match crate::adapter::ouyi_v7_request::from_openai_chat(
        &chat_req,
        model_full.clone(),
        v7_config.max_history_messages,
    ) {
        Ok(i) => i,
        Err(e) => {
            record_responses_error(&state, start_time, &model_full, &e, platform_key);
            return Err(e);
        }
    };

    let client = {
        let ouyi_client = state.ouyi_client.read().await;
        match &*ouyi_client {
            crate::client::OuyiClient::V7(c) => c.clone(),
            crate::client::OuyiClient::V8(_) => {
                let err = AppError::Internal("当前运行客户端不是 7.0".to_string());
                record_responses_error(&state, start_time, &model_full, &err, platform_key);
                return Err(err);
            }
        }
    };

    let text = match client.chat_once(&ouyi_input).await {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("7.0 聊天请求失败: {}", e);
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_full,
                &msg,
                platform_key,
            );
            return Err(AppError::Upstream(msg));
        }
    };

    let pipeline_out = crate::tool_runtime::pipeline::run_openai_tool_pipeline(
        &state,
        &chat_req,
        text,
        "ouyi7",
        start_time,
    )
    .await;

    let short_model = chat_req.model.clone();
    let task_id = format!("resp_{}", crate::tool_runtime::uuid_short());
    let is_stream = req.stream;

    if is_stream {
        let events = responses_stream::events_for_pipeline_output(
            &task_id, &short_model, &pipeline_out,
        );
        let sse_events: Vec<Result<axum::response::sse::Event, axum::Error>> =
            events.into_iter().map(Ok).collect();
        let sse = axum::response::sse::Sse::new(tokio_stream::iter(sse_events));
        Ok(sse.into_response())
    } else {
        let resp_json = responses_non_stream::build_responses_response_from_pipeline(
            &task_id, &short_model, &pipeline_out, 0, 0,
        );
        Ok(Json(resp_json).into_response())
    }
}

async fn handle_v7_responses_stream(
    text: &str,
    model: &str,
    model_full: String,
    state: Arc<AppState>,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    use crate::adapter::ouyi_v7_response::build_responses_stream_from_plain;

    let config = state.config.read().await;
    let chunk_chars = config
        .ouyi_v7
        .as_ref()
        .map(|v| v.simulated_stream_chunk_chars)
        .unwrap_or(80);

    let task_id = format!("resp_{}", uuid_short());
    let response = match build_responses_stream_from_plain(model, &task_id, text, chunk_chars) {
        Ok(response) => response,
        Err(e) => {
            let platform_key = state.platform.read().await.config_key();
            record_responses_error(&state, start_time, &model_full, &e, platform_key);
            return Err(e);
        }
    };

    let stream_elapsed = start_time.elapsed().as_millis() as u64;
    let platform_key = state.platform.read().await.config_key();
    state
        .stats
        .record_success(stream_elapsed, 0, 0, &model_full, platform_key);

    Ok(response)
}

async fn handle_v7_responses_non_stream(
    text: &str,
    model: &str,
    model_full: String,
    state: Arc<AppState>,
    elapsed: u64,
    platform_key: &str,
) -> Result<axum::response::Response, AppError> {
    use crate::adapter::ouyi_v7_response::build_responses_response_from_plain;

    let task_id = format!("resp_{}", uuid_short());
    let mut response = build_responses_response_from_plain(model, &task_id, text);
    // 7.0 平台无法获取准确的 usage
    response.usage = ResponseUsage {
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
    };

    state
        .stats
        .record_success(elapsed, 0, 0, &model_full, platform_key);

    Ok(Json(response).into_response())
}

async fn handle_responses_stream(
    resp: reqwest::Response,
    task_id: String,
    model: String,
    model_full: String,
    session_id: i64,
    client: OuyiHttpClient,
    state: Arc<AppState>,
    start_time: std::time::Instant,
) -> Result<axum::response::Response, AppError> {
    let byte_stream = resp.bytes_stream();
    let guarded = GuardedStream::new(
        byte_stream.map(|result| result.map_err(std::io::Error::other)),
        session_id,
        client,
    );

    let task_id_for_init = task_id.clone();
    let model_for_stream = model.clone();
    let model_for_stats = model_full.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, axum::Error>>(32);

    tokio::spawn(async move {
        let mut sse_buffer = SseLineBuffer::new();
        let mut partial = Vec::new();
        let mut text_buffer = String::new();
        let mut prompt_tokens: u64 = 0;
        let mut completion_tokens: u64 = 0;
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let msg_id = format!("msg_{}", uuid_short());

        let created_event = serde_json::json!({
            "type": "response.created",
            "response": {
                "id": task_id_for_init,
                "object": "response",
                "created_at": created,
                "model": model_for_stream,
                "output": [],
                "status": "in_progress"
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.created")
                .data(created_event.to_string())))
            .await;

        let output_item_added = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": msg_id,
                "status": "in_progress",
                "role": "assistant",
                "content": []
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.output_item.added")
                .data(output_item_added.to_string())))
            .await;

        let content_part_added = serde_json::json!({
            "type": "response.content_part.added",
            "item_id": msg_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
                "annotations": []
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.content_part.added")
                .data(content_part_added.to_string())))
            .await;

        futures::pin_mut!(guarded);
        let mut stream_error: Option<String> = None;
        let mut downstream_closed = false;

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
                if let Some(event) = parse_sse_line(&line) {
                    let event_type =
                        crate::client::sse::OuyiEventType::from(event.event_type.as_str());
                    match event_type {
                        crate::client::sse::OuyiEventType::StringChunk => {
                            if let Some(data) = &event.data {
                                if let Some(t) = data.as_str() {
                                    text_buffer.push_str(t);
                                    let delta = serde_json::json!({
                                        "type": "response.output_text.delta",
                                        "item_id": msg_id,
                                        "output_index": 0,
                                        "content_index": 0,
                                        "delta": t
                                    });
                                    if tx
                                        .send(Ok(Event::default()
                                            .event("response.output_text.delta")
                                            .data(delta.to_string())))
                                        .await
                                        .is_err()
                                    {
                                        downstream_closed = true;
                                        break 'read_loop;
                                    }
                                }
                            }
                        }
                        crate::client::sse::OuyiEventType::Object => {
                            if let Some(data) = &event.data {
                                if let Ok(usage) = serde_json::from_value::<
                                    crate::model::ouyi::OuyiUsage,
                                >(data.clone())
                                {
                                    prompt_tokens = usage.prompt_tokens;
                                    completion_tokens = usage.completion_tokens;
                                }
                            }
                        }
                        crate::client::sse::OuyiEventType::Done => {
                            if let Some(data) = &event.data {
                                if let Ok(usage) = serde_json::from_value::<
                                    crate::model::ouyi::OuyiUsage,
                                >(data.clone())
                                {
                                    prompt_tokens = usage.prompt_tokens;
                                    completion_tokens = usage.completion_tokens;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if downstream_closed {
            state.active_sessions.remove(&session_id);
            return;
        }

        if let Some(msg) = stream_error {
            state.stats.record_error(
                start_time.elapsed().as_millis() as u64,
                &model_for_stats,
                &msg,
                "ouyi8",
            );
            state.active_sessions.remove(&session_id);
            let failed = serde_json::json!({
                "type": "response.failed",
                "response": {
                    "id": &task_id_for_init,
                    "object": "response",
                    "created_at": created,
                    "model": &model_for_stream,
                    "status": "failed",
                    "error": {
                        "message": msg,
                        "type": "stream_error"
                    }
                }
            });
            let _ = tx
                .send(Ok(Event::default()
                    .event("response.failed")
                    .data(failed.to_string())))
                .await;
            return;
        }

        let output_text_done = serde_json::json!({
            "type": "response.output_text.done",
            "item_id": msg_id,
            "output_index": 0,
            "content_index": 0,
            "text": text_buffer
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.output_text.done")
                .data(output_text_done.to_string())))
            .await;

        let content_part_done = serde_json::json!({
            "type": "response.content_part.done",
            "item_id": msg_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": text_buffer,
                "annotations": []
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.content_part.done")
                .data(content_part_done.to_string())))
            .await;

        let output_item_done = serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": msg_id,
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": text_buffer,
                    "annotations": []
                }]
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.output_item.done")
                .data(output_item_done.to_string())))
            .await;

        let completed = serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": task_id_for_init,
                "object": "response",
                "created_at": created,
                "model": model_for_stream,
                "output": [{
                    "type": "message",
                    "id": msg_id,
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": text_buffer,
                        "annotations": []
                    }]
                }],
                "usage": {
                    "input_tokens": prompt_tokens,
                    "output_tokens": completion_tokens,
                    "total_tokens": prompt_tokens + completion_tokens
                }
            }
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("response.completed")
                .data(completed.to_string())))
            .await;

        let stream_elapsed = start_time.elapsed().as_millis() as u64;
        state.stats.record_success(
            stream_elapsed,
            prompt_tokens,
            completion_tokens,
            &model_for_stats,
            "ouyi8",
        );
        state.active_sessions.remove(&session_id);
    });

    let stream = ReceiverStream::new(rx);
    let sse = Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    );

    Ok(sse.into_response())
}

async fn handle_responses_non_stream(
    resp: reqwest::Response,
    task_id: String,
    model: String,
    model_full: String,
    session_id: i64,
    client: OuyiHttpClient,
    state: Arc<AppState>,
    elapsed: u64,
) -> Result<axum::response::Response, AppError> {
    let mut collector = SseCollector::new(model.clone(), format!("chatcmpl-{}", uuid_short()));
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

    while let Some(chunk_result) = guarded.next().await {
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
                collector.push_event(&event);
            }
        }
    }

    if let Some(remaining) = sse_buffer.drain_remaining()
        && let Some(event) = parse_sse_line(&remaining)
    {
        collector.push_event(&event);
    }

    if let Some(msg) = stream_error {
        guarded.as_mut().skip_cleanup();
        spawn_session_cleanup(client, session_id);
        state.active_sessions.remove(&session_id);
        state
            .stats
            .record_error(elapsed, &model_full, &msg, "ouyi8");
        return Err(AppError::SseStream(msg));
    }

    let (prompt, completion) = collector.tokens();
    let chat_response = collector.build_response();

    guarded.as_mut().skip_cleanup();
    spawn_session_cleanup(client, session_id);
    state.active_sessions.remove(&session_id);
    state
        .stats
        .record_success(elapsed, prompt, completion, &model_full, "ouyi8");

    let response = build_responses_response(&model, task_id, chat_response);
    Ok(Json(response).into_response())
}

fn convert_responses_to_chat(req: &ResponsesRequest) -> Result<ChatCompletionRequest, AppError> {
    let messages = match &req.input {
        serde_json::Value::String(text) => {
            let mut msgs = Vec::new();
            if let Some(instructions) = &req.instructions {
                msgs.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(instructions.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            msgs.push(ChatMessage {
                role: "user".to_string(),
                content: Some(text.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
            msgs
        }
        serde_json::Value::Array(items) => {
            let mut msgs: Vec<ChatMessage> = Vec::new();

            if let Some(instructions) = &req.instructions {
                msgs.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(instructions.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }

            for item in items {
                let role = item
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string();

                let content = match role.as_str() {
                    "developer" => {
                        let c = extract_text_content(item.get("content"));
                        let mut msg = ChatMessage {
                            role: "system".to_string(),
                            content: Some(c),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        };
                        if msgs.first().map(|m| m.role.as_str()) == Some("system") {
                            let existing = msgs[0].content.take().unwrap_or_default();
                            msgs[0].content =
                                Some(format!("{}\n\n{}", existing, msg.content.take().unwrap()));
                            continue;
                        }
                        msgs.push(msg);
                        continue;
                    }
                    _ => extract_text_content(item.get("content")),
                };

                msgs.push(ChatMessage {
                    role,
                    content: Some(content),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            msgs
        }
        _ => {
            return Err(AppError::BadRequest(
                "input 字段必须是字符串或消息数组".to_string(),
            ));
        }
    };

    if messages.is_empty() {
        return Err(AppError::BadRequest("消息列表为空".to_string()));
    }

    Ok(ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        stream: false,
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
        tools: req
            .tools
            .as_ref()
            .map(|tools| convert_responses_tools(tools)),
        tool_choice: None,
        parallel_tool_calls: None,
    })
}

fn convert_responses_tools(tools: &[serde_json::Value]) -> Vec<ToolDef> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let parameters = tool.get("parameters").cloned();
            Some(ToolDef {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: name.to_string(),
                    description,
                    parameters,
                },
            })
        })
        .collect()
}

fn extract_text_content(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("input_text") {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn build_responses_response(
    model: &str,
    task_id: String,
    chat_response: ChatCompletionResponse,
) -> ResponsesResponse {
    let choice = &chat_response.choices[0];
    let text = choice.message.content.clone().unwrap_or_default();

    let msg_id = format!("msg_{}", uuid_short());
    let output = ResponseOutputItem {
        item_type: "message".to_string(),
        id: msg_id,
        call_id: None,
        status: "completed".to_string(),
        role: "assistant".to_string(),
        name: None,
        arguments: None,
        content: vec![ResponseContentPart {
            content_type: "output_text".to_string(),
            text,
            annotations: Vec::new(),
        }],
    };

    ResponsesResponse {
        id: task_id,
        object: "response".to_string(),
        created_at: chat_response.created,
        model: model.to_string(),
        output: vec![output],
        usage: ResponseUsage {
            input_tokens: chat_response.usage.prompt_tokens,
            output_tokens: chat_response.usage.completion_tokens,
            total_tokens: chat_response.usage.total_tokens,
        },
    }
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
    use axum::Json;
    use axum::extract::State;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::admin::stats::StatsCollector;
    use crate::admin::{AccountStore, auth::AdminStore};
    use crate::client::ouyi_v7::Ouyi7HttpClient;
    use crate::config::{
        AdminConfig, AppConfig, LimitsConfig, Ouyi7Config, OuyiConfig, PlatformConfig,
        ServerConfig, ToolCallConfig,
    };
    use crate::model::mapper::ModelMapper;

    fn make_test_state(config: AppConfig) -> Arc<AppState> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mapper = ModelMapper::new();
        mapper.load_fallback();
        let platform = config
            .platform
            .as_ref()
            .map(|p| p.default)
            .unwrap_or(OuyiPlatform::Ouyi8);
        let base_url = config.ouyi.base_url.clone();

        Arc::new(AppState {
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

    fn test_config(ouyi_base_url: String) -> AppConfig {
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
            },
            admin: Some(AdminConfig {
                jwt_secret: Some("test-secret".to_string()),
                password_hash: None,
            }),
            tool_call: ToolCallConfig::default(),
            tool_runtime: None,
            openai_tool_runtime: None,
            platform: Some(PlatformConfig {
                default: OuyiPlatform::Ouyi8,
                prompt_on_start: false,
            }),
            ouyi_v7: None,
        }
    }

    fn make_v7_test_state(ouyi_base_url: String) -> Arc<AppState> {
        let http_client = reqwest::Client::builder()
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
            },
            admin: Some(AdminConfig {
                jwt_secret: Some("test-secret".to_string()),
                password_hash: None,
            }),
            tool_call: ToolCallConfig::default(),
            tool_runtime: None,
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
    async fn test_ouyi7_responses_tools_rejection_records_error_and_clears_active_request() {
        let state = make_v7_test_state("http://127.0.0.1:1".to_string());
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: json!("hello"),
            instructions: None,
            stream: false,
            tools: Some(vec![json!({
                "type": "function",
                "name": "Bash",
                "description": "Run shell command",
                "parameters": {"type": "object"}
            })]),
            temperature: None,
            max_output_tokens: None,
        };

        let err = responses(State(state.clone()), Json(req))
            .await
            .unwrap_err();

        // 7.0 + tools 现在走 responses_v7_tools 流水线：尝试连接不可达 base url
        // 时会在聊天请求阶段失败（AppError::Upstream），而不是返回 BadRequest。
        assert!(matches!(err, AppError::Upstream(_)) || matches!(err, AppError::Internal(_)));
        assert_eq!(state.stats.stats.active_requests.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_ouyi8_responses_with_tools_routes_to_pipeline_path() {
        // 8.0 + tools 现在走 responses_v8_tools 流水线：尝试连接不可达 base url
        // 时会在创建会话阶段失败（AppError::Upstream），而不是返回 BadRequest。
        let state = make_test_state(test_config("http://127.0.0.1:1".to_string()));
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: json!("hello"),
            instructions: None,
            stream: false,
            tools: Some(vec![json!({
                "type": "function",
                "name": "Bash",
                "description": "Run shell command",
                "parameters": {"type": "object"}
            })]),
            temperature: None,
            max_output_tokens: None,
        };

        let err = responses(State(state.clone()), Json(req))
            .await
            .unwrap_err();

        // 不再是 BadRequest；上游不可达导致 Upstream 错误
        assert!(matches!(err, AppError::Upstream(_)) || matches!(err, AppError::Internal(_)));
        assert_eq!(state.stats.stats.active_requests.load(Ordering::Relaxed), 0);
    }
}
