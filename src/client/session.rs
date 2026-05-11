use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::Stream;
use pin_project_lite::pin_project;

use crate::client::http::OuyiHttpClient;

pin_project! {
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

pub fn spawn_session_cleanup(client: OuyiHttpClient, session_id: i64) {
    tokio::spawn(async move {
        tracing::debug!(session_id, "非流式响应清理会话");
        if let Err(e) = client.delete_session(session_id).await {
            tracing::warn!(session_id, error = %e, "会话清理失败");
        }
    });
}
