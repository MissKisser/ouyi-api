//! 推理入口的 API Key 鉴权模块：凭据提取、key 规范化、掩码与常数时间比较。
//! [`require_api_key`] 作为推理与模型列表路由的最外层中间件：`[api_keys]`
//! 配置了非空 key 列表时，请求必须携带命中白名单的 API Key
//! （`Authorization: Bearer` 或 `x-api-key`）或有效的管理端 JWT；
//! 否则按路径协议族返回 401 错误信封。未配置时全部放行。

/// API Key 的最小长度；更短的 key 在管理端保存时被整体拒绝。
pub const MIN_KEY_LEN: usize = 16;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use crate::AppState;

/// 提取 Authorization 头中的 Bearer 凭据：auth-scheme 按 RFC 7235 不区分大小写，
/// 头形如 `Bearer <key>` 时返回去前缀并去空白后的 key，其余形式返回 `None`。
fn bearer_credential(value: &str) -> Option<&str> {
    value
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("Bearer "))
        .map(|_| value[7..].trim())
}

/// 从请求头提取调用凭据：优先 `Authorization: Bearer <key>`（scheme 不区分大小写），
/// 其次 `x-api-key: <key>`。两处均缺失或值为空白时返回 `None`。
pub(crate) fn extract_credential(req: &Request) -> Option<String> {
    let headers = req.headers();
    if let Some(key) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_credential)
        .filter(|key| !key.is_empty())
    {
        return Some(key.to_string());
    }
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

/// 判断 `[api_keys]` 鉴权当前是否启用：配置了非空 key 列表。
async fn auth_enabled(state: &Arc<AppState>) -> bool {
    state
        .config
        .read()
        .await
        .api_keys
        .as_ref()
        .is_some_and(|section| !section.keys.is_empty())
}

/// 判断请求凭据是否通过鉴权：命中任一已配置 API Key，或为有效管理端 JWT。
/// `credential` 为调用方在发起异步校验前从请求头同步提取的凭据；
/// 鉴权未启用时直接放行。
async fn is_authorized(state: &Arc<AppState>, credential: Option<&str>) -> bool {
    if !auth_enabled(state).await {
        return true;
    }

    let Some(credential) = credential else {
        return false;
    };

    let key_hit = {
        let config = state.config.read().await;
        config
            .api_keys
            .as_ref()
            .map(|section| {
                section
                    .keys
                    .iter()
                    .any(|key| constant_time_eq(key, credential))
            })
            .unwrap_or(false)
    };
    if key_hit {
        return true;
    }

    let store = state.admin_store.read().await;
    crate::admin::auth::verify_jwt(&store, credential)
}

/// 推理入口鉴权中间件：未通过鉴权的请求按路径协议族返回 401——
/// `/anthropic` 前缀返回 Anthropic 风格 `authentication_error` 信封，
/// 其余返回 OpenAI 风格 `invalid_api_key` 信封。
/// 拒绝发生在占用并发名额之前，不计入并发统计。
pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let credential = extract_credential(&req);
    if !is_authorized(&state, credential.as_deref()).await {
        let path = req.uri().path();
        tracing::warn!(path, "401 unauthorized inference request");
        let body = if path.starts_with("/anthropic") {
            json!({
                "type": "error",
                "error": {
                    "type": "authentication_error",
                    "message": "Invalid API key"
                }
            })
        } else {
            json!({
                "error": {
                    "message": "Invalid API key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })
        };
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }
    next.run(req).await
}

/// 规范化 API Key 列表：逐项去空白；空白项与掩码占位项被忽略，
/// 过滤后是否保留既有 key 由调用方按更新语义决定；
/// 短于 [`MIN_KEY_LEN`] 的项使整次更新失败；结果去重。
/// 返回 `Err(String)`：描述第一个过短的项。
pub fn normalize_keys(raw: &[String]) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = Vec::new();
    for item in raw {
        let key = item.trim();
        if key.is_empty() || is_masked(key) {
            continue;
        }
        if key.len() < MIN_KEY_LEN {
            return Err(format!(
                "API key 长度不足 {MIN_KEY_LEN} 个字符: {}",
                mask_key(key)
            ));
        }
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    Ok(keys)
}

/// 判断值是否为掩码占位值：包含 `…` 或 `****` 的项视为表示保留已有 key 的占位，跳过更新。
fn is_masked(value: &str) -> bool {
    value.contains('…') || value.contains("****")
}

/// 生成脱敏展示形式：保留前 3 与后 4 个字符，中间以 `…` 连接；
/// 长度不足 9 个字符时整体以 `****` 代替。
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// 常数时间字符串比较，避免 key 比对被计时侧信道利用。
/// 长度不同直接返回 `false`（key 长度不视为秘密）。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keys_trims_dedupes_and_rejects_short() {
        let ok = normalize_keys(&[
            "  sk-alpha-0123456789abcdef  ".to_string(),
            "sk-alpha-0123456789abcdef".to_string(),
        ])
        .unwrap();
        assert_eq!(ok, vec!["sk-alpha-0123456789abcdef".to_string()]);

        let err = normalize_keys(&["sk-short".to_string()]).unwrap_err();
        assert!(err.contains("长度不足"));
    }

    #[test]
    fn normalize_keys_ignores_blank_and_masked_entries() {
        let ok = normalize_keys(&[
            String::new(),
            "   ".to_string(),
            "sk-…6789".to_string(),
            "sk-beta-0123456789abcdef".to_string(),
        ])
        .unwrap();
        assert_eq!(ok, vec!["sk-beta-0123456789abcdef".to_string()]);
    }

    #[test]
    fn mask_key_keeps_head_and_tail() {
        assert_eq!(mask_key("sk-alpha-0123456789abcdef"), "sk-…cdef");
        assert_eq!(mask_key("short"), "****");
    }

    #[test]
    fn constant_time_eq_matches_only_identical_values() {
        assert!(constant_time_eq(
            "sk-a-0123456789abcdef",
            "sk-a-0123456789abcdef"
        ));
        assert!(!constant_time_eq(
            "sk-a-0123456789abcdef",
            "sk-b-0123456789abcdef"
        ));
        assert!(!constant_time_eq("short", "sk-a-0123456789abcdef"));
    }

    #[test]
    fn extract_credential_prefers_bearer_then_x_api_key() {
        let mut req = axum::extract::Request::builder()
            .uri("/v1/models")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(extract_credential(&req), None);

        req.headers_mut().insert(
            "authorization",
            axum::http::HeaderValue::from_static("Bearer sk-carrier-0123456789"),
        );
        assert_eq!(
            extract_credential(&req),
            Some("sk-carrier-0123456789".to_string())
        );

        let mut req2 = axum::extract::Request::builder()
            .uri("/anthropic/v1/messages")
            .body(axum::body::Body::empty())
            .unwrap();
        req2.headers_mut().insert(
            "x-api-key",
            axum::http::HeaderValue::from_static("sk-xheader-0123456789"),
        );
        assert_eq!(
            extract_credential(&req2),
            Some("sk-xheader-0123456789".to_string())
        );

        let mut req3 = axum::extract::Request::builder()
            .uri("/v1/models")
            .body(axum::body::Body::empty())
            .unwrap();
        req3.headers_mut().insert(
            "authorization",
            axum::http::HeaderValue::from_static("bearer sk-lowerhdr-0123456789"),
        );
        assert_eq!(
            extract_credential(&req3),
            Some("sk-lowerhdr-0123456789".to_string())
        );
    }
}
