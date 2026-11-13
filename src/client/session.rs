//! 上游会话生命周期守护：包装流式响应，确保流结束后（含中途丢弃）
//! 自动删除上游会话，防止会话泄漏。

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::Stream;
use pin_project_lite::pin_project;

use crate::client::http::OuyiHttpClient;

pin_project! {
    /// 带 Drop 守护的流包装：被丢弃时异步删除关联的上游会话
    /// （已通过 [`GuardedStream::skip_cleanup`] 跳过时除外）。
    pub struct GuardedStream<S> {
        #[pin]
        inner: S,
        session_id: i64,
        client: OuyiHttpClient,
        cleaned_up: bool,
    }

    impl<S> PinnedDrop for GuardedStream<S> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            if *this.cleaned_up {
                return;
            }
            *this.cleaned_up = true;

            let session_id = *this.session_id;
            let client = this.client.clone();

            tokio::spawn(async move {
                tracing::debug!(session_id, "异步清理会话");
                if let Err(e) = client.delete_session(session_id).await {
                    tracing::warn!(session_id, error = %e, "会话清理失败");
                }
            });
        }
    }
}

impl<S> GuardedStream<S> {
    /// 包装底层流。
    ///
    /// * `inner` - 被包装的上游响应流
    /// * `session_id` - 流结束后要删除的上游会话 ID
    /// * `client` - 用于调用删除接口的客户端克隆
    pub fn new(inner: S, session_id: i64, client: OuyiHttpClient) -> Self {
        Self {
            inner,
            session_id,
            client,
            cleaned_up: false,
        }
    }

    /// 标记跳过 Drop 时的自动清理，用于需要保留会话的场景（如 tool_use 响应缓存）
    pub fn skip_cleanup(self: Pin<&mut Self>) {
        *self.project().cleaned_up = true;
    }
}

impl<S, T> Stream for GuardedStream<S>
where
    S: Stream<Item = T>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().inner.poll_next(cx)
    }
}

/// 后台异步删除上游会话（用于非流式响应路径）。
///
/// 副作用：spawn 一个 tokio 任务，向上游发起 DELETE 请求；删除失败仅记
/// 告警日志。
pub fn spawn_session_cleanup(client: OuyiHttpClient, session_id: i64) {
    tokio::spawn(async move {
        tracing::debug!(session_id, "非流式响应清理会话");
        if let Err(e) = client.delete_session(session_id).await {
            tracing::warn!(session_id, error = %e, "会话清理失败");
        }
    });
}
