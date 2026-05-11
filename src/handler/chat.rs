use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use axum::Json;
use futures::stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::adapter::request::convert_request;
use crate::adapter::response::{SseCollector, StreamConverter};
use crate::client::http::OuyiHttpClient;
use crate::client::session::{spawn_session_cleanup, GuardedStream};
use crate::client::sse::{parse_sse_line, SseLineBuffer};
use crate::error::AppError;
use crate::model::openai::ChatCompletionRequest;
use crate::AppState;

pub async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, AppError> {
    // 请求大小检查
    let messages_size: usize = req
        .messages
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len())
        .sum();
    if messages_size > state.config.limits.max_messages_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "消息总大小 {} 超过限制 {}",
            messages_size,
            state.config.limits.max_messages_bytes
        )));
    }

    // 模型名映射
    let model_full = state.model_mapper.to_ouyi(&req.model);
    let should_thinking = state.model_mapper.should_thinking(&req.model);

    state.stats.record_start();
    let start_time = std::time::Instant::now();

    // 请求转换
    let ouyi_req = convert_request(&req, &model_full, should_thinking)
        .map_err(AppError::Internal)?;

    // 优先从账号池获取 token，否则使用配置的 JWT
    let jwt_token = match state.account_store.get_next_token().await {
        Some(token) => token,
        None => state.config.ouyi.jwt_token.clone(),
    };

    if jwt_token.is_empty() {
        return Err(AppError::BadRequest("未配置 JWT token 且无可用账号 token".to_string()));
    }

    // 创建HTTP客户端
    let client = OuyiHttpClient::new(
        state.http_client.clone(),
        state.config.ouyi.base_url.clone(),
        jwt_token,
        state.config.ouyi.app_version.clone(),
    );

    // 创建会话
    let session_data = match client.create_session(&model_full).await {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("创建会话失败: {}", e);
            state.stats.record_error(start_time.elapsed().as_millis() as u64, &model_full, &msg);
            return Err(AppError::Upstream(msg));
        }
    };
    let session_id = session_data.id;

    state.active_sessions.insert(session_id, ());

    // 构建最终文本（system prompt 合并到消息中）
    let final_text = if let Some(sys_prompt) = &ouyi_req.system_prompt {
        format!("[System]: {}\n\n{}", sys_prompt, ouyi_req.text)
    } else {
        ouyi_req.text.clone()
    };

    // 发送聊天请求
    let resp = match client
        .chat_stream(session_id, final_text, ouyi_req.thinking)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            state.active_sessions.remove(&session_id);
            return Err(AppError::Upstream(format!("聊天请求失败: {}", e)));
        }
    };

    let elapsed = start_time.elapsed().as_millis() as u64;
    if req.stream {
        handle_stream_response(resp, model_full, session_id, client, state.clone(), start_time).await
    } else {
        handle_non_stream_response(resp, model_full, session_id, client, state.clone(), elapsed).await
    }
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
        futures::pin_mut!(guarded);

        while let Some(chunk_result) = guarded.next().await {
            let chunk = match chunk_result {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::error!(error = %e, "SSE字节流读取错误");
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
                if let Some(event) = parse_sse_line(&line)
                    && let Some(chunks) = converter.convert_event(&event)
                {
                    for chunk in chunks {
                        let json = match serde_json::to_string(&chunk) {
                            Ok(j) => j,
                            Err(_) => continue,
                        };
                        let sse_event = Event::default().data(json);
                        if tx.send(Ok(sse_event)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }

        // 处理剩余缓冲
        if let Some(remaining) = sse_buffer.drain_remaining()
            && let Some(event) = parse_sse_line(&remaining)
            && let Some(chunks) = converter.convert_event(&event)
        {
            for chunk in chunks {
                let json = match serde_json::to_string(&chunk) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                let sse_event = Event::default().data(json);
                let _ = tx.send(Ok(sse_event)).await;
            }
        }

        let (prompt, completion) = converter.tokens();
        let stream_elapsed = start_time.elapsed().as_millis() as u64;
        state.stats.record_success(stream_elapsed, prompt, completion, &model_for_stats);
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
    let max_bytes = state.config.limits.max_response_bytes;
    let mut partial = Vec::new();

    while let Some(chunk_result) = guarded.next().await {
        let chunk = match chunk_result {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(error = %e, "SSE字节流读取错误");
                break;
            }
        };

        total_bytes += chunk.len();
        if total_bytes > max_bytes {
            tracing::warn!(total_bytes, max_bytes, "响应超过大小限制");
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

    spawn_session_cleanup(client, session_id);
    state.active_sessions.remove(&session_id);
    state.stats.record_success(elapsed, prompt, completion, &model);

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
