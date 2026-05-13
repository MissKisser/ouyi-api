use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct AccountEntry {
    pub email: String,
    pub mobile: String,
    pub area_code: String,
    pub password: String,
    pub token: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountState {
    Idle,
    Busy,
    Error { count: u32 },
    Invalid,
}

#[derive(Debug, Clone)]
pub struct AccountRuntime {
    pub state: AccountState,
    pub last_used_at: Option<std::time::SystemTime>,
    pub request_count: u64,
}

pub struct AccountStore {
    accounts: RwLock<Vec<AccountEntry>>,
    runtime: RwLock<HashMap<String, AccountRuntime>>,
    next_index: AtomicUsize,
}

impl AccountStore {
    pub fn new() -> Self {
        Self {
            accounts: RwLock::new(Vec::new()),
            runtime: RwLock::new(HashMap::new()),
            next_index: AtomicUsize::new(0),
        }
    }

    /// 原子 check-and-add：写锁临界区内检查重复并添加
    pub async fn add_account(&self, entry: AccountEntry) -> Result<(), String> {
        let mut accounts = self.accounts.write().await;
        if accounts.iter().any(|a| a.email == entry.email) {
            return Err("Account already exists".to_string());
        }
        accounts.push(entry);
        Ok(())
    }

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

    pub async fn get_accounts(&self) -> Vec<AccountEntry> {
        self.accounts.read().await.clone()
    }

    pub async fn get_runtime_stats(&self) -> Vec<(String, AccountRuntime)> {
        self.runtime
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// 获取下一个可用 token（轮询）
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
                AccountState::Idle => true,
                AccountState::Error { count } if count < 3 => true,
                _ => false,
            };

            if should_use {
                rt.state = AccountState::Busy;
                return Some(token.clone());
            }
        }

        None
    }

    pub async fn mark_success(&self, email: &str) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(email) {
            rt.state = AccountState::Idle;
            rt.request_count += 1;
            rt.last_used_at = Some(std::time::SystemTime::now());
        }
    }

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

    pub async fn mark_invalid(&self, email: &str) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(email) {
            rt.state = AccountState::Invalid;
        }
    }

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolStats {
    pub total: usize,
    pub enabled: usize,
    pub idle: usize,
    pub busy: usize,
    pub error: usize,
    pub invalid: usize,
}
