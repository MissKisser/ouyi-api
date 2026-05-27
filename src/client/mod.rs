pub mod http;
pub mod ouyi_v7;
pub mod session;
pub mod sse;

use crate::platform::OuyiPlatform;

/// 平台客户端枚举
#[derive(Clone)]
pub enum OuyiClient {
    V7(ouyi_v7::Ouyi7HttpClient),
    V8(http::OuyiHttpClient),
}

impl OuyiClient {
    pub fn platform(&self) -> OuyiPlatform {
        match self {
            OuyiClient::V7(_) => OuyiPlatform::Ouyi7,
            OuyiClient::V8(_) => OuyiPlatform::Ouyi8,
        }
    }
}
