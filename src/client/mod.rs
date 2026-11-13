//! 上游客户端层：按平台封装对 ouyi 7.0 / 8.0 服务的 HTTP 访问，
//! 以及 SSE 解析、跨分块 UTF-8 解码与会话清理守护等流式处理辅助。
//!
//! - [`http`][]: ouyi 8.0 上游客户端
//! - [`ouyi_v7`][]: ouyi 7.0 上游客户端与模型路由
//! - [`session`][]: 上游会话的自动清理守护
//! - [`sse`][]: SSE 事件解析与敏感信息脱敏

/// ouyi 8.0 上游 HTTP 客户端。
pub mod http;
/// ouyi 7.0 上游 HTTP 客户端与模型路由。
pub mod ouyi_v7;
/// 上游会话的自动清理守护。
pub mod session;
/// SSE 事件解析与敏感信息脱敏。
pub mod sse;

use crate::platform::OuyiPlatform;

/// 平台客户端枚举：按当前活动平台分发到对应的上游 HTTP 客户端实现。
#[derive(Clone)]
pub enum OuyiClient {
    /// ouyi 7.0 平台客户端。
    V7(ouyi_v7::Ouyi7HttpClient),
    /// ouyi 8.0 平台客户端。
    V8(http::OuyiHttpClient),
}

impl OuyiClient {
    /// 返回该客户端对应的平台标识。
    pub fn platform(&self) -> OuyiPlatform {
        match self {
            OuyiClient::V7(_) => OuyiPlatform::Ouyi7,
            OuyiClient::V8(_) => OuyiPlatform::Ouyi8,
        }
    }
}
