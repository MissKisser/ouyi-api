//! 上游账号池存储：维护账号静态条目与各自的运行时状态。
//!
//! [`AccountStore`] 以邮箱为主键管理账号列表，`get_next_token` 按轮询
//! 选取可用账号并跳过禁用、失效或连续出错的账号；`mark_*` 方法由请求
//! 处理路径回调以反馈单次调用的成败，`pool_stats` 汇总供管理后台展示。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;

/// 单个上游账号的静态配置条目。
#[derive(Debug, Clone)]
pub struct AccountEntry {
    /// 账号登录邮箱，作为账号池内的唯一主键。
    pub email: String,
    /// 账号绑定的手机号。
    pub mobile: String,
    /// 手机号所属区号（如 "86"）。
    pub area_code: String,
    /// 账号密码的 bcrypt 哈希；为空串表示未设置密码。
    pub password: String,
    /// 该账号的 JWT 凭据；`None` 表示尚未持有可用 token。
    pub token: Option<String>,
    /// 是否参与轮询调度；为 `false` 时 `get_next_token` 会跳过该账号。
    pub enabled: bool,
}

/// 账号的运行时健康状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountState {
    /// 空闲可用：最近一次调用成功或尚未被调用。
    Idle,
    /// 正在处理请求。
    Busy,
    /// 连续出错，`count` 为累计错误次数；达到 3 次后会被轮询跳过。
    Error { count: u32 },
    /// 凭据已失效，被轮询跳过。
    Invalid,
}

/// 单个账号的运行时统计（随请求调度动态更新）。
#[derive(Debug, Clone)]
pub struct AccountRuntime {
    /// 当前健康状态。
    pub state: AccountState,
    /// 最近一次被选中使用的时刻；从未被使用过则为 `None`。
    pub last_used_at: Option<std::time::SystemTime>,
    /// 累计被选中使用的次数。
    pub request_count: u64,
}

/// 账号池共享存储，供管理后台与请求处理路径并发访问。
pub struct AccountStore {
    accounts: RwLock<Vec<AccountEntry>>,
    runtime: RwLock<HashMap<String, AccountRuntime>>,
    next_index: AtomicUsize,
}

impl AccountStore {
    /// 创建空账号池；账号需通过 [`Self::set_accounts`] 或 [`Self::add_account`] 载入。
    pub fn new() -> Self {
        Self {
            accounts: RwLock::new(Vec::new()),
            runtime: RwLock::new(HashMap::new()),
            next_index: AtomicUsize::new(0),
        }
    }

    /// 向账号池追加一个账号。
    ///
    /// 在写锁临界区内先检查邮箱是否重复再插入，保证检查与添加原子完成。
    ///
    /// 返回 `Err("Account already exists")` 表示同邮箱账号已存在，
    /// 此时池内容不变。
    pub async fn add_account(&self, entry: AccountEntry) -> Result<(), String> {
        let mut accounts = self.accounts.write().await;
        if accounts.iter().any(|a| a.email == entry.email) {
            return Err("Account already exists".to_string());
        }
        accounts.push(entry);
        Ok(())
    }

    /// 用给定列表整体替换账号池内容。
    ///
    /// 已存在账号的运行时状态会保留迁移；新出现的账号以 `Idle` 状态、
    /// 零计数初始化，被移除账号的运行时状态随之丢弃。
    pub async fn set_accounts(&self, entries: Vec<AccountEntry>) {
        let mut accounts = self.accounts.write().await;
        let mut runtime = self.runtime.write().await;

        let mut new_runtime = HashMap::new();
        for entry in &entries {
            let key = entry.email.clone();
            if let Some(old) = runtime.remove(&key) {
                new_runtime.insert(key, old);
            } else {
                new_runtime.insert(
                    key,
                    AccountRuntime {
                        state: AccountState::Idle,
                        last_used_at: None,
                        request_count: 0,
                    },
                );
            }
        }

        *accounts = entries;
        *runtime = new_runtime;
    }

    /// 返回全部账号条目的快照（深拷贝）。
    pub async fn get_accounts(&self) -> Vec<AccountEntry> {
        self.accounts.read().await.clone()
    }

    /// 返回各账号的运行时状态快照，元素为 `(邮箱, 运行时统计)`。
    pub async fn get_runtime_stats(&self) -> Vec<(String, AccountRuntime)> {
        self.runtime
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// 轮询选取下一个可用账号并返回其 JWT token。
    ///
    /// 跳过未启用、无 token、状态为 `Invalid` 或连续出错达 3 次的账号；
    /// 选中后递增该账号的请求计数并刷新最近使用时间。
    ///
    /// 池为空或所有账号均不可用时返回 `None`。
    pub async fn get_next_token(&self) -> Option<String> {
        let accounts = self.accounts.read().await;
        let len = accounts.len();
        if len == 0 {
            return None;
        }

        let mut runtime = self.runtime.write().await;
        for _ in 0..len {
            let idx = self.next_index.fetch_add(1, Ordering::SeqCst) % len;
            let account = &accounts[idx];

            if !account.enabled {
                continue;
            }

            let token = account.token.as_ref()?;
            let rt = match runtime.get_mut(&account.email) {
                Some(rt) => rt,
                None => continue,
            };

            let should_use = match rt.state {
                AccountState::Invalid => false,
                AccountState::Error { count } if count >= 3 => false,
                _ => true,
            };

            if should_use {
                rt.request_count += 1;
                rt.last_used_at = Some(std::time::SystemTime::now());
                return Some(token.clone());
            }
        }

        None
    }

    /// 将指定邮箱账号标记为一次成功调用：状态复位为 `Idle`，
    /// 并累加请求计数、刷新最近使用时间。账号不存在时无操作。
    pub async fn mark_success(&self, email: &str) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(email) {
            rt.state = AccountState::Idle;
            rt.request_count += 1;
            rt.last_used_at = Some(std::time::SystemTime::now());
        }
    }

    /// 将指定邮箱账号标记为一次失败调用：错误计数加 1；计数达到 3 后
    /// 该账号会被 `get_next_token` 跳过。账号不存在时无操作。
    pub async fn mark_error(&self, email: &str) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(email) {
            let count = match rt.state {
                AccountState::Error { count } => count + 1,
                _ => 1,
            };
            rt.state = AccountState::Error { count };
        }
    }

    /// 将指定邮箱账号标记为凭据失效，此后被轮询跳过。账号不存在时无操作。
    pub async fn mark_invalid(&self, email: &str) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(email) {
            rt.state = AccountState::Invalid;
        }
    }

    /// 汇总账号池的健康分布，供管理后台状态页展示。
    pub async fn pool_stats(&self) -> PoolStats {
        let accounts = self.accounts.read().await;
        let runtime = self.runtime.read().await;

        let mut total = 0;
        let mut enabled = 0;
        let mut idle = 0;
        let mut busy = 0;
        let mut error = 0;
        let mut invalid = 0;

        for account in accounts.iter() {
            total += 1;
            if account.enabled {
                enabled += 1;
            }

            if let Some(rt) = runtime.get(&account.email) {
                match rt.state {
                    AccountState::Idle => idle += 1,
                    AccountState::Busy => busy += 1,
                    AccountState::Error { .. } => error += 1,
                    AccountState::Invalid => invalid += 1,
                }
            }
        }

        PoolStats {
            total,
            enabled,
            idle,
            busy,
            error,
            invalid,
        }
    }
}

/// 账号池状态汇总快照（可序列化为 JSON 供管理后台展示）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolStats {
    /// 账号总数。
    pub total: usize,
    /// 已启用的账号数。
    pub enabled: usize,
    /// 处于 `Idle` 状态的账号数。
    pub idle: usize,
    /// 处于 `Busy` 状态的账号数。
    pub busy: usize,
    /// 处于出错状态的账号数。
    pub error: usize,
    /// 凭据失效的账号数。
    pub invalid: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(email: &str, token: &str) -> AccountEntry {
        AccountEntry {
            email: email.to_string(),
            mobile: String::new(),
            area_code: String::new(),
            password: String::new(),
            token: Some(token.to_string()),
            enabled: true,
        }
    }

    #[tokio::test]
    async fn get_next_token_keeps_jwt_account_reusable() {
        let store = AccountStore::new();
        store
            .set_accounts(vec![account("a@example.com", "jwt-a")])
            .await;

        assert_eq!(store.get_next_token().await.as_deref(), Some("jwt-a"));
        assert_eq!(store.get_next_token().await.as_deref(), Some("jwt-a"));

        let stats = store.pool_stats().await;
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.busy, 0);
    }
}
