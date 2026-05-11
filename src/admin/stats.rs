use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use dashmap::DashMap;

/// 单个请求的统计快照
pub struct RequestStats {
    pub total_requests: AtomicU64,
    pub active_requests: AtomicUsize,
    pub successful_requests: AtomicU64,
    pub failed_requests: AtomicU64,
    pub total_latency_ms: AtomicU64,
    pub total_prompt_tokens: AtomicU64,
    pub total_completion_tokens: AtomicU64,
}

impl Default for RequestStats {
    fn default() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            active_requests: AtomicUsize::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            total_latency_ms: AtomicU64::new(0),
            total_prompt_tokens: AtomicU64::new(0),
            total_completion_tokens: AtomicU64::new(0),
        }
    }
}

/// 运行日志条目
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub details: Option<String>,
}

/// 请求日志条目
#[derive(Clone, Debug)]
pub struct RequestLog {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub status: String,
    pub latency_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub error: Option<String>,
}

/// 全局统计数据容器
pub struct StatsCollector {
    pub stats: Arc<RequestStats>,
    request_logs: DashMap<u64, RequestLog>,
    next_log_id: AtomicU64,
    runtime_logs: DashMap<u64, LogEntry>,
    next_runtime_id: AtomicU64,
    start_time: Instant,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(RequestStats::default()),
            request_logs: DashMap::new(),
            next_log_id: AtomicU64::new(0),
            runtime_logs: DashMap::new(),
            next_runtime_id: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    pub fn record_start(&self) {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_success(&self, latency_ms: u64, prompt_tokens: u64, completion_tokens: u64, model: &str) {
        self.stats.active_requests.fetch_sub(1, Ordering::Relaxed);
        self.stats.successful_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
        self.stats.total_prompt_tokens.fetch_add(prompt_tokens, Ordering::Relaxed);
        self.stats.total_completion_tokens.fetch_add(completion_tokens, Ordering::Relaxed);

        self.add_request_log(RequestLog {
            timestamp: Utc::now(),
            model: model.to_string(),
            status: "200".to_string(),
            latency_ms,
            prompt_tokens,
            completion_tokens,
            error: None,
        });
    }

    pub fn record_error(&self, latency_ms: u64, model: &str, error: &str) {
        self.stats.active_requests.fetch_sub(1, Ordering::Relaxed);
        self.stats.failed_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);

        self.add_request_log(RequestLog {
            timestamp: Utc::now(),
            model: model.to_string(),
            status: "500".to_string(),
            latency_ms,
            prompt_tokens: 0,
            completion_tokens: 0,
            error: Some(error.to_string()),
        });
    }

    pub fn add_runtime_log(&self, entry: LogEntry) {
        let id = self.next_runtime_id.fetch_add(1, Ordering::Relaxed);
        self.runtime_logs.insert(id, entry);
        if self.runtime_logs.len() > 500 {
            let min_id = id.saturating_sub(500);
            self.runtime_logs.retain(|k, _| *k >= min_id);
        }
    }

    pub fn get_request_logs(&self, limit: usize) -> Vec<RequestLog> {
        let mut entries: Vec<_> = self.request_logs.iter().map(|r| r.value().clone()).collect();
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries.truncate(limit);
        entries
    }

    pub fn get_runtime_logs(&self, limit: usize) -> Vec<LogEntry> {
        let mut entries: Vec<_> = self.runtime_logs.iter().map(|r| r.value().clone()).collect();
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries.truncate(limit);
        entries
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    fn add_request_log(&self, log: RequestLog) {
        let id = self.next_log_id.fetch_add(1, Ordering::Relaxed);
        self.request_logs.insert(id, log);
        if self.request_logs.len() > 1000 {
            let min_id = id.saturating_sub(1000);
            self.request_logs.retain(|k, _| *k >= min_id);
        }
    }
}
