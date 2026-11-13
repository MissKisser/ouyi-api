//! 管理后台鉴权：bcrypt 密码哈希校验、JWT 签发与验证、按 IP 的登录限流。
//!
//! [`AdminStore`] 在内存中持有 JWT 签名密钥、管理员密码哈希与登录尝试
//! 计数；[`verify_jwt`] 供鉴权中间件校验请求携带的 Bearer token。

use std::time::Instant;

use bcrypt::{DEFAULT_COST, hash, verify};
use dashmap::DashMap;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// 内存存储 admin 鉴权凭据与登录限流状态。
pub(crate) struct AdminStore {
    /// JWT 签名/验签密钥。
    pub(crate) jwt_secret: String,
    /// 管理员密码的 bcrypt 哈希；为空串表示尚未完成初始化设置。
    pub(crate) password_hash: String,
    /// 是否已完成初始化（已设置管理员密码）。
    pub(crate) setup_completed: bool,
    /// 按 IP 记录的登录尝试计数，用于登录限流。
    login_attempts: DashMap<String, LoginAttempt>,
}

impl AdminStore {
    /// 用给定 JWT 密钥与密码哈希构建存储。
    ///
    /// `password_hash` 为空串时视为未完成初始化（`setup_completed = false`）。
    pub(crate) fn new(jwt_secret: String, password_hash: String) -> Self {
        Self {
            jwt_secret,
            setup_completed: !password_hash.is_empty(),
            password_hash,
            login_attempts: DashMap::new(),
        }
    }

    /// 以 bcrypt 默认成本对明文密码做哈希。
    ///
    /// 返回 `Err` 表示 bcrypt 哈希失败（如输入超出实现限制）。
    pub(crate) fn hash_password(raw: &str) -> Result<String, bcrypt::BcryptError> {
        hash(raw, DEFAULT_COST)
    }

    /// 校验明文密码是否与存储的哈希匹配；哈希校验出错时按不匹配处理
    /// （返回 `false`）。
    pub(crate) fn verify_password(&self, raw: &str) -> bool {
        verify(raw, &self.password_hash).unwrap_or(false)
    }

    /// 替换管理员密码哈希，并将存储标记为已完成初始化设置。
    pub(crate) fn set_password_hash(&mut self, password_hash: String) {
        self.password_hash = password_hash;
        self.setup_completed = true;
    }

    /// 为给定主体（写入 claims 的 `sub` 字段）签发有效期 24 小时的 JWT。
    ///
    /// 返回 `Err` 表示 JWT 编码失败。
    pub(crate) fn generate_token(&self, sub: &str) -> Result<String, jsonwebtoken::errors::Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize;

        let claims = Claims {
            sub: sub.to_string(),
            exp: now + 86400,
            iat: now,
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )
    }

    /// 登录限流：同一 IP 在 5 分钟窗口内最多允许 5 次登录尝试，
    /// 超出窗口后重新计数。
    ///
    /// 每次调用都会为该 IP 的计数加 1（含本次）；返回 `false` 表示
    /// 本次登录尝试应被拒绝。
    pub(crate) fn check_rate_limit(&self, ip: &str) -> bool {
        const MAX_ATTEMPTS: usize = 5;
        const WINDOW_SECS: u64 = 300;

        let now = Instant::now();
        let mut entry = self
            .login_attempts
            .entry(ip.to_string())
            .or_insert_with(|| LoginAttempt {
                count: 0,
                first_seen: now,
            });

        if now.duration_since(entry.first_seen).as_secs() > WINDOW_SECS {
            entry.count = 0;
            entry.first_seen = now;
        }

        entry.count += 1;
        entry.count <= MAX_ATTEMPTS
    }
}

/// 校验 token 是否为使用存储密钥签发且未过期的有效 JWT。
/// 格式非法、签名不匹配或已过期均返回 `false`。
pub(crate) fn verify_jwt(store: &AdminStore, token: &str) -> bool {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(store.jwt_secret.as_bytes()),
        &Validation::default(),
    );
    token_data.is_ok()
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    iat: usize,
}

struct LoginAttempt {
    count: usize,
    first_seen: Instant,
}
