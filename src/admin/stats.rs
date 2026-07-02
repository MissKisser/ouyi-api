use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    pub platform: String,
}

/// 工具运行时日志条目
#[derive(Clone, Debug)]
pub struct ToolRuntimeLog {
    pub timestamp: DateTime<Utc>,
    pub request_id: String,
    pub endpoint: String,
    pub mode: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub decision: String,
    pub tool_names: Vec<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub latency_ms: u64,
    pub success: bool,
    pub error: Option<String>,
}

/// 工具运行时决策类型
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRuntimeDecision {
    ToolCalls,
    FinalAnswer,
}

impl ToolRuntimeDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolRuntimeDecision::ToolCalls => "tool_calls",
            ToolRuntimeDecision::FinalAnswer => "final_answer",
        }
    }
}

/// 工具运行时模式
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRuntimeMode {
    Compiler,
    Broker,
}

impl ToolRuntimeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolRuntimeMode::Compiler => "compiler",
            ToolRuntimeMode::Broker => "broker",
        }
    }
}

/// 模型统计条目
#[derive(Debug, Default)]
pub struct ModelStats {
    pub requests: AtomicU64,
    pub prompt_tokens: AtomicU64,
    pub completion_tokens: AtomicU64,
}

/// 全局统计数据容器
pub struct StatsCollector {
    pub stats: Arc<RequestStats>,
    request_logs: DashMap<u64, RequestLog>,
    next_log_id: AtomicU64,
    runtime_logs: DashMap<u64, LogEntry>,
    next_runtime_id: AtomicU64,
    tool_runtime_logs: DashMap<u64, ToolRuntimeLog>,
    next_tool_runtime_log_id: AtomicU64,
    model_stats: DashMap<String, ModelStats>,
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
            tool_runtime_logs: DashMap::new(),
            next_tool_runtime_log_id: AtomicU64::new(0),
            model_stats: DashMap::new(),
            start_time: Instant::now(),
        }
    }

    pub fn record_start(&self) {
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        self.stats.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn finish_active_request(&self) {
        let _ = self.stats.active_requests.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| current.checked_sub(1),
        );
    }

    pub fn record_success(
        &self,
        latency_ms: u64,
        prompt_tokens: u64,
        completion_tokens: u64,
        model: &str,
        platform: &str,
    ) {
        self.finish_active_request();
        self.stats
            .successful_requests
            .fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);
        self.stats
            .total_prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.stats
            .total_completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);

        self.add_request_log(RequestLog {
            timestamp: Utc::now(),
            model: model.to_string(),
            status: "200".to_string(),
            latency_ms,
            prompt_tokens,
            completion_tokens,
            error: None,
            platform: platform.to_string(),
        });
    }

    pub fn record_error(&self, latency_ms: u64, model: &str, error: &str, platform: &str) {
        self.finish_active_request();
        self.stats.failed_requests.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);

        self.add_request_log(RequestLog {
            timestamp: Utc::now(),
            model: model.to_string(),
            status: "500".to_string(),
            latency_ms,
            prompt_tokens: 0,
            completion_tokens: 0,
            error: Some(error.to_string()),
            platform: platform.to_string(),
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
        let mut entries: Vec<_> = self
            .request_logs
            .iter()
            .map(|r| r.value().clone())
            .collect();
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries.truncate(limit);
        entries
    }

    pub fn get_runtime_logs(&self, limit: usize) -> Vec<LogEntry> {
        let mut entries: Vec<_> = self
            .runtime_logs
            .iter()
            .map(|r| r.value().clone())
            .collect();
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

    /// 记录工具运行时成功调用
    pub fn record_tool_runtime_success(
        &self,
        request_id: &str,
        endpoint: &str,
        mode: ToolRuntimeMode,
        provider: &str,
        model: &str,
        base_url: &str,
        decision: ToolRuntimeDecision,
        tool_names: Vec<String>,
        prompt_tokens: u64,
        completion_tokens: u64,
        latency_ms: u64,
    ) {
        // 累加到系统总 Token
        self.stats
            .total_prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.stats
            .total_completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);

        // 写入模型统计（使用 tool-runtime::<model> 格式）
        let model_key = format!("tool-runtime::{}", model);
        self.update_model_stats(&model_key, prompt_tokens, completion_tokens);

        // 记录日志
        let log = ToolRuntimeLog {
            timestamp: Utc::now(),
            request_id: request_id.to_string(),
            endpoint: endpoint.to_string(),
            mode: mode.as_str().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: mask_base_url(base_url),
            decision: decision.as_str().to_string(),
            tool_names,
            prompt_tokens,
            completion_tokens,
            latency_ms,
            success: true,
            error: None,
        };
        self.add_tool_runtime_log(log);
    }

    /// 记录工具运行时失败调用
    pub fn record_tool_runtime_error(
        &self,
        request_id: &str,
        endpoint: &str,
        mode: ToolRuntimeMode,
        provider: &str,
        model: &str,
        base_url: &str,
        decision: ToolRuntimeDecision,
        tool_names: Vec<String>,
        prompt_tokens: u64,
        completion_tokens: u64,
        latency_ms: u64,
        error: &str,
    ) {
        // 累加到系统总 Token（即使失败也可能消耗 Token）
        self.stats
            .total_prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.stats
            .total_completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);

        // 写入模型统计（使用 tool-runtime::<model> 格式）
        let model_key = format!("tool-runtime::{}", model);
        self.update_model_stats(&model_key, prompt_tokens, completion_tokens);

        // 记录日志
        let log = ToolRuntimeLog {
            timestamp: Utc::now(),
            request_id: request_id.to_string(),
            endpoint: endpoint.to_string(),
            mode: mode.as_str().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: mask_base_url(base_url),
            decision: decision.as_str().to_string(),
            tool_names,
            prompt_tokens,
            completion_tokens,
            latency_ms,
            success: false,
            error: Some(error.to_string()),
        };
        self.add_tool_runtime_log(log);
    }

    /// 获取工具运行时日志
    pub fn get_tool_runtime_logs(&self, limit: usize) -> Vec<ToolRuntimeLog> {
        let mut entries: Vec<_> = self
            .tool_runtime_logs
            .iter()
            .map(|r| r.value().clone())
            .collect();
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries.truncate(limit);
        entries
    }

    /// 获取模型统计
    pub fn get_model_stats(&self) -> Vec<(String, u64, u64, u64)> {
        self.model_stats
            .iter()
            .map(|entry| {
                let key = entry.key().clone();
                let stats = entry.value();
                (
                    key,
                    stats.requests.load(Ordering::Relaxed),
                    stats.prompt_tokens.load(Ordering::Relaxed),
                    stats.completion_tokens.load(Ordering::Relaxed),
                )
            })
            .collect()
    }

    /// 更新模型统计
    fn update_model_stats(&self, model_key: &str, prompt_tokens: u64, completion_tokens: u64) {
        self.model_stats
            .entry(model_key.to_string())
            .and_modify(|stats| {
                stats.requests.fetch_add(1, Ordering::Relaxed);
                stats
                    .prompt_tokens
                    .fetch_add(prompt_tokens, Ordering::Relaxed);
                stats
                    .completion_tokens
                    .fetch_add(completion_tokens, Ordering::Relaxed);
            })
            .or_insert_with(|| {
                let stats = ModelStats::default();
                stats.requests.store(1, Ordering::Relaxed);
                stats.prompt_tokens.store(prompt_tokens, Ordering::Relaxed);
                stats
                    .completion_tokens
                    .store(completion_tokens, Ordering::Relaxed);
                stats
            });
    }

    /// 添加工具运行时日志
    fn add_tool_runtime_log(&self, log: ToolRuntimeLog) {
        let id = self
            .next_tool_runtime_log_id
            .fetch_add(1, Ordering::Relaxed);
        self.tool_runtime_logs.insert(id, log);
        if self.tool_runtime_logs.len() > 1000 {
            let min_id = id.saturating_sub(1000);
            self.tool_runtime_logs.retain(|k, _| *k >= min_id);
        }
    }
}

/// 脱敏 base_url，隐藏协议和域名部分
fn mask_base_url(url: &str) -> String {
    if url.is_empty() {
        return url.to_string();
    }
    // 简化脱敏：保留前 15 个字符（包含协议和部分域名），其余脱敏
    if url.len() > 15 {
        format!("{}...***", &url[..15])
    } else if url.len() > 5 {
        format!("{}***", &url[..5])
    } else {
        "***".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn record_error_after_success_does_not_underflow_active_requests() {
        let collector = StatsCollector::new();

        collector.record_start();
        collector.record_success(10, 1, 1, "model-a", "ouyi8");
        collector.record_error(11, "model-a", "late error", "ouyi8");

        assert_eq!(collector.stats.active_requests.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_tool_runtime_success_updates_tokens_logs_and_model_stats() {
        let collector = StatsCollector::new();

        collector.record_tool_runtime_success(
            "msg_1",
            "anthropic",
            ToolRuntimeMode::Compiler,
            "openai-compatible",
            "cheap-tool-model",
            "https://api.example.com/v1",
            ToolRuntimeDecision::ToolCalls,
            vec!["Read".to_string()],
            11,
            3,
            120,
        );

        assert_eq!(
            collector.stats.total_prompt_tokens.load(Ordering::Relaxed),
            11
        );
        assert_eq!(
            collector
                .stats
                .total_completion_tokens
                .load(Ordering::Relaxed),
            3
        );
        assert_eq!(collector.get_tool_runtime_logs(10).len(), 1);
        let stats = collector.get_model_stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "tool-runtime::cheap-tool-model");
        assert_eq!(stats[0].1, 1);
        assert_eq!(stats[0].2, 11);
        assert_eq!(stats[0].3, 3);
    }
}
