//! 统计采集：全局请求计数、按模型聚合的用量，以及请求日志 / 运行日志 /
//! 工具运行时日志的内存缓存（超容量时淘汰最旧条目），供管理后台的
//! 统计与日志页面读取。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use chrono::{DateTime, Utc};
use dashmap::DashMap;

/// 全局请求统计计数器（原子操作，可跨线程共享）。
pub struct RequestStats {
    /// 累计收到的请求数。
    pub total_requests: AtomicU64,
    /// 当前处理中的请求数。
    pub active_requests: AtomicUsize,
    /// 累计成功完成的请求数。
    pub successful_requests: AtomicU64,
    /// 累计失败的请求数。
    pub failed_requests: AtomicU64,
    /// 全部请求的延迟总和（毫秒），除以总请求数即平均延迟。
    pub total_latency_ms: AtomicU64,
    /// 累计消耗的 prompt token 数。
    pub total_prompt_tokens: AtomicU64,
    /// 累计产出的 completion token 数。
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

/// 运行日志条目。
#[derive(Clone, Debug)]
pub struct LogEntry {
    /// 日志产生时间（UTC）。
    pub timestamp: DateTime<Utc>,
    /// 日志级别（如 "INFO" / "WARN" / "ERROR"）。
    pub level: String,
    /// 日志正文。
    pub message: String,
    /// 可选的附加详情。
    pub details: Option<String>,
}

/// 单次请求的日志条目。
#[derive(Clone, Debug)]
pub struct RequestLog {
    /// 请求完成时间（UTC）。
    pub timestamp: DateTime<Utc>,
    /// 请求使用的模型名。
    pub model: String,
    /// HTTP 状态码字符串（"200" / "500"）。
    pub status: String,
    /// 请求耗时（毫秒）。
    pub latency_ms: u64,
    /// 消耗的 prompt token 数；失败请求为 0。
    pub prompt_tokens: u64,
    /// 产出的 completion token 数；失败请求为 0。
    pub completion_tokens: u64,
    /// 失败时的错误描述；成功请求为 `None`。
    pub error: Option<String>,
    /// 处理该请求的平台标识。
    pub platform: String,
}

/// 单次工具运行时调用的日志条目。
#[derive(Clone, Debug)]
pub struct ToolRuntimeLog {
    /// 记录时间（UTC）。
    pub timestamp: DateTime<Utc>,
    /// 所属请求 ID。
    pub request_id: String,
    /// 请求入口端点。
    pub endpoint: String,
    /// 运行模式（"compiler" / "broker"）。
    pub mode: String,
    /// 工具模型提供方标识。
    pub provider: String,
    /// 工具模型名。
    pub model: String,
    /// 工具模型服务地址（已脱敏）。
    pub base_url: String,
    /// 决策结果（"tool_calls" / "final_answer"）。
    pub decision: String,
    /// 本次涉及的工具名列表。
    pub tool_names: Vec<String>,
    /// 消耗的 prompt token 数。
    pub prompt_tokens: u64,
    /// 产出的 completion token 数。
    pub completion_tokens: u64,
    /// 耗时（毫秒）。
    pub latency_ms: u64,
    /// 调用是否成功。
    pub success: bool,
    /// 失败时的错误描述；成功时为 `None`。
    pub error: Option<String>,
}

/// 工具运行时的最终决策类型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRuntimeDecision {
    /// 判定需要发起工具调用。
    ToolCalls,
    /// 判定可直接产出最终回答。
    FinalAnswer,
}

impl ToolRuntimeDecision {
    /// 返回决策的日志标识串（"tool_calls" / "final_answer"）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolRuntimeDecision::ToolCalls => "tool_calls",
            ToolRuntimeDecision::FinalAnswer => "final_answer",
        }
    }
}

/// 工具运行时的运行模式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRuntimeMode {
    /// 编译器模式：把工具清单编译进对工具模型的请求，按调用协议解析其决策。
    Compiler,
    /// 中介模式：解析主模型输出中携带的工具调用标记，据此产出决策。
    Broker,
}

impl ToolRuntimeMode {
    /// 返回模式的日志标识串（"compiler" / "broker"）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolRuntimeMode::Compiler => "compiler",
            ToolRuntimeMode::Broker => "broker",
        }
    }
}

/// 单个模型的用量统计条目（原子计数）。
#[derive(Debug, Default)]
pub struct ModelStats {
    /// 请求次数。
    pub requests: AtomicU64,
    /// 累计消耗的 prompt token。
    pub prompt_tokens: AtomicU64,
    /// 累计产出的 completion token。
    pub completion_tokens: AtomicU64,
}

/// 全局统计数据容器：持有全局计数与三类日志的内存缓存（按自增 ID 写入，
/// 超容量时淘汰最旧条目），可在多线程间共享。
pub struct StatsCollector {
    /// 全局请求计数器，供热路径直接读取。
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
    /// 创建空统计容器，运行时长从此刻起算。
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

    /// 记录一次请求开始：总请求数与活跃请求数各加 1。
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

    /// 记录一次成功完成：活跃计数减 1，成功数、延迟与 token 总量累加，
    /// 并写入一条 200 请求日志。
    ///
    /// * `latency_ms` - 请求耗时（毫秒）
    /// * `prompt_tokens` / `completion_tokens` - 本次消耗/产出的 token 数
    /// * `model` / `platform` - 模型名与平台标识，用于日志与聚合
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

    /// 记录一次失败请求：活跃计数减 1，失败数与延迟累加，并写入一条
    /// 500 请求日志（token 计数为 0）。
    ///
    /// * `latency_ms` - 请求耗时（毫秒）
    /// * `model` / `platform` - 模型名与平台标识
    /// * `error` - 错误描述，写入日志条目
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

    /// 追加一条运行日志；缓存超过 500 条时淘汰最旧条目。
    pub fn add_runtime_log(&self, entry: LogEntry) {
        let id = self.next_runtime_id.fetch_add(1, Ordering::Relaxed);
        self.runtime_logs.insert(id, entry);
        if self.runtime_logs.len() > 500 {
            let min_id = id.saturating_sub(500);
            self.runtime_logs.retain(|k, _| *k >= min_id);
        }
    }

    /// 返回最多 `limit` 条请求日志，按时间倒序（最新在前）。
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

    /// 返回最多 `limit` 条运行日志，按时间倒序（最新在前）。
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

    /// 返回容器创建至今的运行时长（秒）。
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

    /// 记录一次工具模型成功调用：token 累加进全局总量，写入
    /// `tool-runtime::<model>` 键的模型统计，并追加一条成功日志
    /// （`base_url` 先脱敏再存储）。
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

    /// 记录一次工具模型失败调用：已消耗的 token 仍累加进全局总量与模型
    /// 统计，并追加一条携带错误描述的失败日志。
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

    /// 返回最多 `limit` 条工具运行时日志，按时间倒序（最新在前）。
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

    /// 返回按模型聚合的用量统计，元素为
    /// `(模型键, 请求数, prompt token 总量, completion token 总量)`。
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
