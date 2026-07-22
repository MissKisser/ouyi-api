use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::model::openai::ChatMessage;

#[derive(Debug, Clone)]
struct ResponseHistory {
    messages: Vec<ChatMessage>,
    created_at: Instant,
}

pub struct ResponsesState {
    entries: DashMap<String, ResponseHistory>,
    ttl: Duration,
    capacity: usize,
}

impl Default for ResponsesState {
    fn default() -> Self {
        Self::new(Duration::from_secs(30 * 60), 1024)
    }
}

impl ResponsesState {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: DashMap::new(),
            ttl,
            capacity: capacity.max(1),
        }
    }

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

    pub fn get(&self, id: &str) -> Option<Vec<ChatMessage>> {
        let entry = self.entries.get(id)?;
        if entry.created_at.elapsed() >= self.ttl {
            drop(entry);
            self.entries.remove(id);
            return None;
        }
        Some(entry.messages.clone())
    }

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
