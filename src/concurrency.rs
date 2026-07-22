use std::sync::{Arc, Mutex, MutexGuard};

use axum::Json;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::json;

struct LimiterState {
    max: usize,
    active: usize,
}

#[derive(Clone)]
pub struct ConcurrencyLimiter {
    state: Arc<Mutex<LimiterState>>,
}

pub struct ConcurrencyPermit {
    state: Option<Arc<Mutex<LimiterState>>>,
}

impl ConcurrencyLimiter {
    pub fn new(max: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(LimiterState { max, active: 0 })),
        }
    }

    pub fn try_acquire(&self) -> Option<ConcurrencyPermit> {
        let mut state = lock_state(&self.state);
        if state.max != 0 && state.active >= state.max {
            return None;
        }

        state.active += 1;
        Some(ConcurrencyPermit {
            state: Some(Arc::clone(&self.state)),
        })
    }

    pub fn set_limit(&self, max: usize) {
        lock_state(&self.state).max = max;
    }

    pub fn limit(&self) -> usize {
        lock_state(&self.state).max
    }
}

pub async fn limit_inference_requests(
    State(limiter): State<ConcurrencyLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let is_anthropic = request.uri().path().starts_with("/anthropic");
    let Some(permit) = limiter.try_acquire() else {
        let body = if is_anthropic {
            json!({
                "type": "error",
                "error": {
                    "type": "rate_limit_error",
                    "message": "Global concurrency limit reached"
                }
            })
        } else {
            json!({
                "error": {
                    "message": "Global concurrency limit reached",
                    "type": "rate_limit_error",
                    "code": "concurrency_limit_exceeded"
                }
            })
        };
        let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };

    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    let body = body.map_frame(move |frame| {
        let _ = &permit;
        frame
    });
    Response::from_parts(parts, Body::new(body))
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        let mut state = lock_state(&state);
        if let Some(active) = state.active.checked_sub(1) {
            state.active = active;
        }
    }
}

fn lock_state(state: &Mutex<LimiterState>) -> MutexGuard<'_, LimiterState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::middleware;
    use axum::response::Response;
    use axum::routing::{get, post};
    use bytes::Bytes;
    use serde_json::{Value, json};

    use super::{ConcurrencyLimiter, limit_inference_requests};

    async fn pending_response() -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(futures::stream::pending::<
                Result<Bytes, Infallible>,
            >()))
            .unwrap()
    }

    async fn ok_response() -> StatusCode {
        StatusCode::OK
    }

    fn http_test_app(limiter: ConcurrencyLimiter) -> Router {
        let inference = Router::new()
            .route("/v1/chat/completions", post(pending_response))
            .route("/anthropic/v1/messages", post(pending_response))
            .route_layer(middleware::from_fn_with_state(
                limiter,
                limit_inference_requests,
            ));

        Router::new()
            .route("/v1/models", get(ok_response))
            .route("/admin/status", get(ok_response))
            .merge(inference)
    }

    async fn spawn_http_server(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn http_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[test]
    fn limit_two_rejects_third_permit() {
        let limiter = ConcurrencyLimiter::new(2);

        let _first = limiter.try_acquire().expect("first permit should succeed");
        let _second = limiter.try_acquire().expect("second permit should succeed");

        assert!(limiter.try_acquire().is_none());
    }

    #[test]
    fn dropping_permit_allows_next_acquisition() {
        let limiter = ConcurrencyLimiter::new(1);
        let permit = limiter.try_acquire().expect("permit should succeed");
        assert!(limiter.try_acquire().is_none());

        drop(permit);

        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn zero_limit_allows_multiple_active_permits() {
        let limiter = ConcurrencyLimiter::new(0);
        let mut permits = Vec::new();

        for _ in 0..100 {
            permits.push(
                limiter
                    .try_acquire()
                    .expect("unlimited limiter should always grant permits"),
            );
        }

        assert_eq!(permits.len(), 100);
    }

    #[test]
    fn lowering_limit_keeps_existing_permits_and_rejects_new_ones() {
        let limiter = ConcurrencyLimiter::new(3);
        let first = limiter.try_acquire().expect("first permit should succeed");
        let second = limiter.try_acquire().expect("second permit should succeed");
        let third = limiter.try_acquire().expect("third permit should succeed");

        limiter.set_limit(1);

        assert_eq!(limiter.limit(), 1);
        assert!(limiter.try_acquire().is_none());
        drop(first);
        drop(second);
        assert!(limiter.try_acquire().is_none());
        drop(third);
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn raising_limit_allows_new_permits_immediately() {
        let limiter = ConcurrencyLimiter::new(1);
        let _first = limiter.try_acquire().expect("first permit should succeed");
        assert!(limiter.try_acquire().is_none());

        limiter.set_limit(3);

        assert_eq!(limiter.limit(), 3);
        let _second = limiter.try_acquire().expect("second permit should succeed");
        let _third = limiter.try_acquire().expect("third permit should succeed");
        assert!(limiter.try_acquire().is_none());
    }

    #[test]
    fn poisoned_mutex_is_recovered_for_drop_and_future_acquisitions() {
        let limiter = ConcurrencyLimiter::new(1);
        let permit = limiter.try_acquire().expect("permit should succeed");
        let poison_target = limiter.clone();

        let panic_result = std::thread::spawn(move || {
            let _guard = poison_target.state.lock().unwrap();
            panic!("poison limiter mutex");
        })
        .join();
        assert!(panic_result.is_err());

        drop(permit);

        assert!(limiter.try_acquire().is_some());
    }

    #[tokio::test]
    async fn response_body_owns_permit_until_response_is_dropped() {
        let limiter = ConcurrencyLimiter::new(1);
        let base_url = spawn_http_server(http_test_app(limiter)).await;
        let client = http_client();

        let first = client
            .post(format!("{base_url}/anthropic/v1/messages"))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = client
            .post(format!("{base_url}/v1/chat/completions"))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            second.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("1"))
        );
        let second_body: Value = second.json().await.unwrap();
        assert_eq!(
            second_body,
            json!({
                "error": {
                    "message": "Global concurrency limit reached",
                    "type": "rate_limit_error",
                    "code": "concurrency_limit_exceeded"
                }
            })
        );

        for path in ["/v1/models", "/admin/status"] {
            let response = client
                .get(format!("{base_url}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }

        drop(first);

        let third = client
            .post(format!("{base_url}/v1/chat/completions"))
            .send()
            .await
            .unwrap();
        assert_eq!(third.status(), StatusCode::OK);
        drop(third);
    }

    #[tokio::test]
    async fn anthropic_limit_response_uses_anthropic_error_envelope() {
        let limiter = ConcurrencyLimiter::new(1);
        let _permit = limiter.try_acquire().unwrap();
        let base_url = spawn_http_server(http_test_app(limiter)).await;

        let response = http_client()
            .post(format!("{base_url}/anthropic/v1/messages"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("1"))
        );
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body,
            json!({
                "type": "error",
                "error": {
                    "type": "rate_limit_error",
                    "message": "Global concurrency limit reached"
                }
            })
        );
    }
}
