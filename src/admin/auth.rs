use std::time::Instant;

use bcrypt::{DEFAULT_COST, hash, verify};
use dashmap::DashMap;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// 内存存储 admin 鉴权凭据与登录限流状态
pub(crate) struct AdminStore {
    pub(crate) jwt_secret: String,
    pub(crate) password_hash: String,
    pub(crate) setup_completed: bool,
    login_attempts: DashMap<String, LoginAttempt>,
}

impl AdminStore {
    pub(crate) fn new(jwt_secret: String, password_hash: String) -> Self {
        Self {
            jwt_secret,
            setup_completed: !password_hash.is_empty(),
            password_hash,
            login_attempts: DashMap::new(),
        }
    }

    pub(crate) fn hash_password(raw: &str) -> Result<String, bcrypt::BcryptError> {
        hash(raw, DEFAULT_COST)
    }

    pub(crate) fn verify_password(&self, raw: &str) -> bool {
        verify(raw, &self.password_hash).unwrap_or(false)
    }

    pub(crate) fn set_password_hash(&mut self, password_hash: String) {
        self.password_hash = password_hash;
        self.setup_completed = true;
    }

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

    /// 同一 IP 5 分钟内最多 5 次登录尝试
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
