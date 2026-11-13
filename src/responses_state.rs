//! Responses 协议会话历史缓存模块。
//!
//! 以 `response.id → 会话消息历史` 的形式在内存中缓存 Responses 协议的多轮会话，
//! 供携带前一轮 ID 的续轮请求取回上下文。条目按 TTL 过期，容量满时淘汰最旧条目。

use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::model::openai::ChatMessage;

/// 单条缓存记录：会话消息历史及其写入时刻。
#[derive(Debug, Clone)]
struct ResponseHistory {
    /// 会话的完整消息历史。
    messages: Vec<ChatMessage>,
    /// 写入时刻，用于 TTL 过期与最旧条目淘汰的判定。
    created_at: Instant,
}

/// Responses 会话历史缓存：并发安全的 `id → 消息历史` 映射，带 TTL 与容量上限。
pub struct ResponsesState {
    /// 缓存条目表，key 为 Responses 的会话 ID。
    entries: DashMap<String, ResponseHistory>,
    /// 条目存活时长，超时后视为过期并被移除。
    ttl: Duration,
    /// 最大条目数；容量满时淘汰创建时间最旧的条目，取值下限为 1。
    capacity: usize,
}

impl Default for ResponsesState {
    fn default() -> Self {
        Self::new(Duration::from_secs(30 * 60), 1024)
    }
}

impl ResponsesState {
    /// 创建缓存实例。
    /// - `ttl`: 条目存活时长；
    /// - `capacity`: 最大条目数，传入 `0` 会被抬升为 `1`。
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: DashMap::new(),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// 写入一条会话历史。
    /// 副作用：先清除全部过期条目；若容量已满且 `id` 是新条目，则再淘汰创建时间最旧的条目。
    /// 已存在的 `id` 直接覆盖旧值（容量淘汰只针对新条目）。
    pub fn insert(&self, id: String, messages: Vec<ChatMessage>) {
        self.insert_at(id, messages, Instant::now());
    }

    fn insert_at(&self, id: String, messages: Vec<ChatMessage>, created_at: Instant) {
        self.remove_expired();
        if !self.entries.contains_key(&id)
            && self.entries.len() >= self.capacity
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|entry| entry.created_at)
                .map(|entry| entry.key().clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            id,
            ResponseHistory {
                messages,
                created_at,
            },
        );
    }

    /// 按 ID 取回会话历史（消息列表的克隆副本）。
    /// 命中且未过期时返回 `Some`；命中但已过期时移除该条目并返回 `None`；未命中返回 `None`。
    pub fn get(&self, id: &str) -> Option<Vec<ChatMessage>> {
        let entry = self.entries.get(id)?;
        if entry.created_at.elapsed() >= self.ttl {
            drop(entry);
            self.entries.remove(id);
            return None;
        }
        Some(entry.messages.clone())
    }

    /// 移除所有超过 TTL 的条目。
    fn remove_expired(&self) {
        let ttl = self.ttl;
        self.entries
            .retain(|_, entry| entry.created_at.elapsed() < ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::openai::ChatMessage;
    use std::time::{Duration, Instant};

    fn message(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn returns_unexpired_history_and_isolates_ids() {
        let state = ResponsesState::new(Duration::from_secs(60), 10);
        state.insert("resp_1".to_string(), vec![message("one")]);
        state.insert("resp_2".to_string(), vec![message("two")]);

        assert_eq!(
            state.get("resp_1").unwrap()[0].content.as_deref(),
            Some("one")
        );
        assert_eq!(
            state.get("resp_2").unwrap()[0].content.as_deref(),
            Some("two")
        );
    }

    #[test]
    fn removes_expired_history() {
        let state = ResponsesState::new(Duration::from_secs(1), 10);
        state.insert_at(
            "resp_old".to_string(),
            vec![message("old")],
            Instant::now() - Duration::from_secs(2),
        );

        assert!(state.get("resp_old").is_none());
    }

    #[test]
    fn evicts_oldest_entry_at_capacity() {
        let state = ResponsesState::new(Duration::from_secs(60), 1);
        state.insert_at(
            "resp_1".to_string(),
            vec![message("one")],
            Instant::now() - Duration::from_secs(1),
        );
        state.insert("resp_2".to_string(), vec![message("two")]);

        assert!(state.get("resp_1").is_none());
        assert!(state.get("resp_2").is_some());
    }
}
