//! ouyi 输出到 Anthropic Messages 响应的适配。
//!
//! [`AnthropicStreamConverter`] 把增量文本实时转换为 Anthropic SSE 事件序列
//! （message_start / content_block_* / message_delta / message_stop），识别
//! `<think>` 思考块与工具调用块并生成对应类型的 content block；
//! [`AnthropicCollector`] 聚合完整输出后一次性构造非流式消息。
//! 两条路径均会清理模型自造的 `<tool_result>` 内容并还原 HTML 实体。
use crate::adapter::tool_call::ToolCallParser;
use crate::model::anthropic::*;
use bytes::Bytes;

const THINK_OPEN: &str = "<think";
const THINK_CLOSE: &str = "</thinking>";

/// ouyi 增量文本到 Anthropic 流式 SSE 事件的转换器。
///
/// 内部维护思考块状态、工具调用解析状态与 content block 索引，
/// 保证输出的 start / delta / stop 事件索引与块类型连续一致。
pub struct AnthropicStreamConverter {
    model: String,
    task_id: String,
    tool_parser: ToolCallParser,
    next_block_index: u32,
    current_block_type: Option<BlockType>,
    in_thinking: bool,
    think_buffer: String,
    /// 本次响应中是否已输出过 tool_use 块；置位后忽略后续文本事件。
    pub has_tool_use: bool,
    /// 最近一次输出的 tool_use 块 ID。
    pub last_tool_use_id: Option<String>,
    usage: ResponseUsage,
    pending_text: String,
    /// 是否已输出过任何 content block（含空文本块），用于 done 时兜底空文本。
    pub has_content: bool,
    /// done 收尾是否已处理过，防止重复输出收尾事件序列。
    pub done_processed: bool,
}

#[derive(Clone)]
enum BlockType {
    Thinking,
    Text,
    ToolUse,
}

/// 清理模型输出中的 HTML 实体，还原为标准字符。
/// 上游模型在生成表格等内容时可能输出 &nbsp; 等 HTML 实体，需要网关侧统一适配。
fn unescape_html_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}

/// 扫描字符串尾部，返回可能构成不完整 `<think` 标签前缀的起始位置。
fn find_tag_cutoff(s: &str) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return 0;
    }
    for i in (0..len).rev() {
        if bytes[i] == b'<' {
            let tail = &s[i..];
            if THINK_OPEN.starts_with(tail) {
                return i;
            }
            return len;
        }
        if !bytes[i].is_ascii_alphabetic() {
            return len;
        }
    }
    len
}

/// 剥离文本中模型自造的 `<tool_result>` 块及其包裹标记。
/// 上游模型在决定调用工具时，常会在文本中自行编造 tool_result 内容，
/// 真实的 Anthropic API 不会这样做，需要在响应中清除。
fn strip_fabricated_tool_results(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        // 查找 <tool_result 开始标签
        if let Some(open_pos) = remaining.find("<tool_result") {
            // 保留标签之前的内容（但清除包裹标记）
            let before = &remaining[..open_pos];
            result.push_str(&strip_tool_result_markers(before));
            // 查找 </tool_result> 闭合标签
            if let Some(close_pos) = remaining.find("</tool_result>") {
                remaining = &remaining[close_pos + "</tool_result>".len()..];
            } else {
                // 没有闭合标签，跳过到末尾
                break;
            }
        } else {
            result.push_str(&strip_tool_result_markers(remaining));
            break;
        }
    }
    // 清理多余空行
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    // 仅当结果全为空白字符时返回空字符串，保留换行符以确保表格渲染正确
    if result.chars().all(|c| c.is_whitespace()) {
        return String::new();
    }
    result
}

/// 清除 [Tool Result for ...] 和 [End of tool result for ...] 标记
fn strip_tool_result_markers(text: &str) -> String {
    let mut result = text.to_string();
    loop {
        let changed = false;
        // 移除 [Tool Result for xxx]:
        if let Some(start) = result.find("[Tool Result for ") {
            if let Some(end) = result[start..].find("]:\n") {
                result = format!("{}{}", &result[..start], &result[start + end + 3..]);
                continue;
            }
        }
        // 移除 [End of tool result for xxx.]
        if let Some(start) = result.find("[End of tool result for ") {
            if let Some(end) = result[start..].find(".]") {
                result = format!("{}{}", &result[..start], &result[start + end + 2..]);
                continue;
            }
        }
        if !changed {
            break;
        }
    }
    result
}

/// 查找 `<think>` 标签的起始位置，返回 (标签起始位置, 标签内容开始位置)。
fn find_think_open(s: &str) -> Option<(usize, usize)> {
    let start_pos = s.find(THINK_OPEN)?;
    let after_tag = &s[start_pos + THINK_OPEN.len()..];
    let close_pos = after_tag.find('>')?;
    Some((start_pos, start_pos + THINK_OPEN.len() + close_pos + 1))
}

/// 查找 `</think>` 闭合标签的位置，返回 (标签起始位置, 标签结束位置)。
fn find_think_close(s: &str) -> Option<(usize, usize)> {
    if let Some(start_pos) = s.find("</thinking>") {
        return Some((start_pos, start_pos + "</thinking>".len()));
    }
    let start_pos = s.find("</think>")?;
    Some((start_pos, start_pos + "</think>".len()))
}

/// 扫描 thinking 内容尾部，检查是否有不完整的 `</think` 前缀。
fn find_think_close_cutoff(s: &str) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return 0;
    }
    for i in (0..len).rev() {
        if bytes[i] == b'<' {
            let tail = &s[i..];
            if THINK_CLOSE.starts_with(tail) {
                return i;
            }
            return len;
        }
        if !bytes[i].is_ascii_alphabetic() && bytes[i] != b'/' {
            return len;
        }
    }
    len
}

impl AnthropicStreamConverter {
    /// 创建流式转换器。
    ///
    /// - `model`：写入 message_start 事件的模型名；
    /// - `task_id`：作为消息 ID。
    pub fn new(model: String, task_id: String) -> Self {
        Self {
            model,
            task_id,
            tool_parser: ToolCallParser::new(),
            next_block_index: 0,
            current_block_type: None,
            in_thinking: false,
            think_buffer: String::new(),
            has_tool_use: false,
            last_tool_use_id: None,
            usage: ResponseUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
            pending_text: String::new(),
            has_content: false,
            done_processed: false,
        }
    }

    /// 生成流式响应的 `message_start` 事件字节。
    ///
    /// 携带消息元信息（ID、角色、模型名）与当前用量（初始为零）。
    pub fn start_event(&self) -> Bytes {
        serialize_stream_event(&StreamEvent::MessageStart {
            message: MessageStartInfo {
                r#type: "message_start",
                message: MessageStartMessage {
                    id: self.task_id.clone(),
                    r#type: "message",
                    role: "assistant",
                    model: self.model.clone(),
                    content: vec![],
                    stop_reason: None,
                    stop_sequence: None,
                    usage: self.usage.clone(),
                },
            },
        })
    }

    /// 处理一段 ouyi 增量文本，返回零到多个序列化的 Anthropic SSE 事件字节。
    ///
    /// - `text`：本次到达的文本增量；
    /// - `is_done`：是否为流结束信号；为真时冲刷未闭合的思考块与待发文本、
    ///   在无任何内容时补空文本块、闭合当前块并输出 message_delta / message_stop
    ///   收尾序列（`stop_reason` 为 `tool_use` 或 `end_turn`），重复调用返回空。
    ///
    /// 文本按 `<think>` 开闭边界拆分为 thinking 块与正文块，正文再经工具调用
    /// 解析拆出 tool_use 块；不完整的标记前缀会缓冲到下一轮继续判断。
    /// 副作用：更新内部缓冲、块索引与用量状态。
    pub fn process_ouyi_event(&mut self, text: &str, is_done: bool) -> Vec<Bytes> {
        let mut events: Vec<Bytes> = Vec::new();

        if is_done {
            if self.done_processed {
                return events;
            }
            self.done_processed = true;

            if self.in_thinking && !self.has_tool_use {
                events.extend(self.flush_thinking());
            }
            if !self.has_tool_use {
                events.extend(self.flush_pending_text());
            }
            if !self.has_content {
                events.extend(self.emit_empty_text());
            }
            if self.current_block_type.is_some() {
                let idx = self.next_block_index.saturating_sub(1);
                events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
                    index: idx,
                }));
                self.current_block_type = None;
            }
            let stop_reason = if self.has_tool_use {
                "tool_use"
            } else {
                "end_turn"
            };
            events.push(serialize_stream_event(&StreamEvent::MessageDelta {
                delta: MessageDeltaInfo {
                    stop_reason: stop_reason.to_string(),
                    stop_sequence: None,
                },
                usage: self.usage.clone(),
            }));
            events.push(serialize_stream_event(&StreamEvent::MessageStop));
            return events;
        }

        if text.is_empty() {
            return events;
        }

        if self.has_tool_use {
            return events;
        }

        self.think_buffer.push_str(text);
        let buffer = std::mem::take(&mut self.think_buffer);
        let mut remaining = buffer.as_str();

        while !remaining.is_empty() {
            if self.in_thinking {
                if let Some((close_start, close_end)) = find_think_close(remaining) {
                    let think_content = &remaining[..close_start];
                    if !think_content.is_empty() {
                        events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
                            index: self.next_block_index - 1,
                            delta: DeltaContent::ThinkingDelta {
                                thinking: think_content.to_string(),
                            },
                        }));
                    }
                    let idx = self.next_block_index.saturating_sub(1);
                    events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: DeltaContent::SignatureDelta {
                            signature: generate_signature(),
                        },
                    }));
                    events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
                        index: idx,
                    }));
                    self.current_block_type = None;
                    self.in_thinking = false;
                    remaining = &remaining[close_end..];
                } else {
                    let cutoff = find_think_close_cutoff(remaining);
                    let safe_content = &remaining[..cutoff];
                    let tail = &remaining[cutoff..];
                    if !safe_content.is_empty() {
                        events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
                            index: self.next_block_index - 1,
                            delta: DeltaContent::ThinkingDelta {
                                thinking: safe_content.to_string(),
                            },
                        }));
                    }
                    self.think_buffer = tail.to_string();
                    return events;
                }
            } else if let Some((open_start, content_start)) = find_think_open(remaining) {
                let before = &remaining[..open_start];
                if !before.is_empty() {
                    let parsed = self.tool_parser.process_chunk(before);
                    if !parsed.normal_text.is_empty() {
                        events.extend(self.emit_text(parsed.normal_text));
                    }
                    for delta in parsed.tool_deltas {
                        events.extend(self.emit_tool_start(&delta));
                        events.extend(self.emit_tool_delta(&delta));
                    }
                }
                self.in_thinking = true;
                events.extend(self.emit_thinking_start());
                remaining = &remaining[content_start..];
            } else {
                let cutoff = find_tag_cutoff(remaining);
                if cutoff > 0 {
                    let safe_content = &remaining[..cutoff];
                    let parsed = self.tool_parser.process_chunk(safe_content);
                    if !parsed.normal_text.is_empty() {
                        events.extend(self.emit_text(parsed.normal_text));
                    }
                    for delta in parsed.tool_deltas {
                        events.extend(self.emit_tool_start(&delta));
                        events.extend(self.emit_tool_delta(&delta));
                    }
                }
                let tail = &remaining[cutoff..];
                if tail.is_empty() {
                    return events;
                }
                self.think_buffer = tail.to_string();
                return events;
            }
        }

        events
    }

    fn emit_thinking_start(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        if matches!(self.current_block_type, Some(BlockType::ToolUse)) {
            let idx = self.next_block_index.saturating_sub(1);
            events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
                index: idx,
            }));
            self.current_block_type = None;
        }
        if self.current_block_type.is_some() {
            let idx = self.next_block_index.saturating_sub(1);
            events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
                index: idx,
            }));
            self.current_block_type = None;
        }
        let idx = self.next_block_index;
        events.push(serialize_stream_event(&StreamEvent::ContentBlockStart {
            index: idx,
            content_block: ContentBlockStart::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        }));
        self.current_block_type = Some(BlockType::Thinking);
        self.next_block_index += 1;
        self.has_content = true;
        events
    }

    fn flush_thinking(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        if !self.think_buffer.is_empty() {
            events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
                index: self.next_block_index - 1,
                delta: DeltaContent::ThinkingDelta {
                    thinking: std::mem::take(&mut self.think_buffer),
                },
            }));
        }
        let idx = self.next_block_index.saturating_sub(1);
        events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
            index: idx,
            delta: DeltaContent::SignatureDelta {
                signature: generate_signature(),
            },
        }));
        events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
            index: idx,
        }));
        self.current_block_type = None;
        self.in_thinking = false;
        events
    }

    fn emit_tool_start(&mut self, delta: &crate::model::openai::ToolCallDelta) -> Vec<Bytes> {
        let mut events = Vec::new();
        if let Some(func) = &delta.function {
            self.discard_pending_text();

            if self.current_block_type.is_some() {
                let idx = self.next_block_index.saturating_sub(1);
                events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
                    index: idx,
                }));
                self.current_block_type = None;
            }

            if self.current_block_type.is_none() {
                let idx = self.next_block_index;
                let name = func.name.as_deref().unwrap_or("unknown");
                let id = delta.id.as_deref().unwrap_or("call_0");
                events.push(serialize_stream_event(&StreamEvent::ContentBlockStart {
                    index: idx,
                    content_block: ContentBlockStart::ToolUse {
                        id: id.to_string(),
                        name: name.to_string(),
                        input: serde_json::json!({}),
                    },
                }));
                self.current_block_type = Some(BlockType::ToolUse);
                self.has_tool_use = true;
                self.has_content = true;
                self.last_tool_use_id = Some(id.to_string());
                self.next_block_index += 1;
            }
        }
        events
    }

    fn emit_tool_delta(&mut self, delta: &crate::model::openai::ToolCallDelta) -> Vec<Bytes> {
        let mut events = Vec::new();
        if let Some(func) = &delta.function {
            if let Some(args) = &func.arguments {
                events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
                    index: self.next_block_index - 1,
                    delta: DeltaContent::InputJsonDelta {
                        partial_json: args.clone(),
                    },
                }));
            }
        }
        events
    }

    /// 输出一个空的 text content block（start + stop 事件），用于整次响应
    /// 无任何内容时的兜底，保证客户端能收到合法的内容序列。
    pub fn emit_empty_text(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        let idx = self.next_block_index;
        events.push(serialize_stream_event(&StreamEvent::ContentBlockStart {
            index: idx,
            content_block: ContentBlockStart::Text {
                text: String::new(),
            },
        }));
        self.next_block_index += 1;
        self.has_content = true;
        events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
            index: idx,
        }));
        events
    }

    /// 更新 token 用量，随 message_delta 事件透出。
    ///
    /// - `input_tokens`：输入 token 数；
    /// - `output_tokens`：输出 token 数。
    pub fn update_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.usage.input_tokens = input_tokens;
        self.usage.output_tokens = output_tokens;
    }

    /// 返回当前 token 用量 `(input_tokens, output_tokens)`。
    pub fn tokens(&self) -> (u64, u64) {
        (self.usage.input_tokens, self.usage.output_tokens)
    }

    fn emit_text(&mut self, text: String) -> Vec<Bytes> {
        if self.has_tool_use {
            return Vec::new();
        }

        let cleaned = strip_fabricated_tool_results(&text);
        let cleaned = unescape_html_entities(&cleaned);
        if cleaned.is_empty() {
            return Vec::new();
        }

        self.pending_text.push_str(&cleaned);
        return self.flush_pending_text();
    }

    fn flush_pending_text(&mut self) -> Vec<Bytes> {
        if self.pending_text.is_empty() {
            return Vec::new();
        }

        let text = std::mem::take(&mut self.pending_text);
        let mut events = Vec::new();

        if matches!(self.current_block_type, Some(BlockType::ToolUse))
            || matches!(self.current_block_type, Some(BlockType::Thinking))
        {
            let idx = self.next_block_index.saturating_sub(1);
            events.push(serialize_stream_event(&StreamEvent::ContentBlockStop {
                index: idx,
            }));
            self.current_block_type = None;
        }

        if self.current_block_type.is_none() {
            let idx = self.next_block_index;
            events.push(serialize_stream_event(&StreamEvent::ContentBlockStart {
                index: idx,
                content_block: ContentBlockStart::Text {
                    text: String::new(),
                },
            }));
            self.current_block_type = Some(BlockType::Text);
            self.next_block_index += 1;
        }

        events.push(serialize_stream_event(&StreamEvent::ContentBlockDelta {
            index: self.next_block_index - 1,
            delta: DeltaContent::TextDelta { text },
        }));

        self.has_content = true;
        events
    }

    fn discard_pending_text(&mut self) {
        self.pending_text.clear();
    }
}

fn generate_signature() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("EqoBCkIIC{}", base64_encode_signature(ts))
}

fn base64_encode_signature(n: u128) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let mut n = n;
    for _ in 0..32 {
        bytes.push(CHARSET[(n % 64) as usize]);
        n /= 64;
    }
    String::from_utf8(bytes).unwrap_or_default()
}

// ── Non-streaming Collector ───────────────────────────────────────────────

/// ouyi SSE 事件聚合器，收集完整输出后构造 Anthropic 非流式 Messages 响应。
pub struct AnthropicCollector {
    content_parts: Vec<String>,
    model: String,
    task_id: String,
    tool_parser: ToolCallParser,
    usage: Option<crate::model::ouyi::OuyiUsage>,
}

impl AnthropicCollector {
    /// 创建收集器。
    ///
    /// - `model`：写入响应体的模型名；
    /// - `task_id`：作为消息 ID。
    pub fn new(model: String, task_id: String) -> Self {
        Self {
            content_parts: Vec::new(),
            model,
            task_id,
            tool_parser: ToolCallParser::new(),
            usage: None,
        }
    }

    /// 累积一个 ouyi SSE 事件。
    ///
    /// string 事件追加文本片段；object 事件尝试读取并覆盖 token 用量；
    /// 其余事件类型忽略。
    pub fn push_event(&mut self, event: &crate::model::ouyi::OuyiSseEvent) {
        let event_type = crate::client::sse::OuyiEventType::from(event.event_type.as_str());
        match event_type {
            crate::client::sse::OuyiEventType::StringChunk => {
                if let Some(data) = &event.data
                    && let Some(text) = data.as_str()
                    && !text.is_empty()
                {
                    self.content_parts.push(text.to_string());
                }
            }
            crate::client::sse::OuyiEventType::Object => {
                if let Some(data) = &event.data
                    && let Ok(usage) =
                        serde_json::from_value::<crate::model::ouyi::OuyiUsage>(data.clone())
                {
                    self.usage = Some(usage);
                }
            }
            _ => {}
        }
    }

    /// 返回捕获的 token 用量 `(input_tokens, output_tokens)`；未捕获时为 `(0, 0)`。
    pub fn tokens(&self) -> (u64, u64) {
        match &self.usage {
            Some(u) => (u.prompt_tokens, u.completion_tokens),
            None => (0, 0),
        }
    }

    /// 构造 Anthropic 非流式 Messages 响应。
    ///
    /// 对全部文本按 `<think>` 边界与工具调用标记拆分为 thinking / text /
    /// tool_use 块（顺序保持出现次序）；存在 tool_use 块时丢弃 text 块并把
    /// `stop_reason` 置为 `tool_use`，否则为 `end_turn`；无任何内容时输出一个
    /// 空 text 块；token 用量未捕获时填 0。消费收集器自身。
    pub fn build_response(mut self) -> MessagesResponse {
        let full_text: String = self.content_parts.join("");

        let mut content_blocks: Vec<ResponseContentBlock> = Vec::new();
        let mut remaining = full_text.as_str();

        while !remaining.is_empty() {
            if let Some((open_start, content_start)) = find_think_open(remaining) {
                let before = &remaining[..open_start];
                if !before.is_empty() {
                    let parsed = self.tool_parser.process_chunk(before);
                    if !parsed.normal_text.is_empty() {
                        let cleaned = strip_fabricated_tool_results(&parsed.normal_text);
                        if !cleaned.is_empty() {
                            content_blocks.push(ResponseContentBlock::Text { text: cleaned });
                        }
                    }
                    for delta in parsed.tool_deltas {
                        if let Some(func) = &delta.function {
                            let input: serde_json::Value = func
                                .arguments
                                .as_deref()
                                .and_then(|a| serde_json::from_str(a).ok())
                                .unwrap_or(serde_json::Value::Object(Default::default()));
                            content_blocks.push(ResponseContentBlock::ToolUse {
                                id: delta.id.clone().unwrap_or_default(),
                                name: func.name.clone().unwrap_or_default(),
                                input,
                            });
                        }
                    }
                }
                if let Some((close_start, close_end)) =
                    find_think_close(&remaining[content_start..])
                {
                    let think_content = &remaining[content_start..content_start + close_start];
                    content_blocks.push(ResponseContentBlock::Thinking {
                        thinking: think_content.to_string(),
                        signature: generate_signature(),
                    });
                    remaining = &remaining[content_start + close_end..];
                } else {
                    let think_content = &remaining[content_start..];
                    content_blocks.push(ResponseContentBlock::Thinking {
                        thinking: think_content.to_string(),
                        signature: generate_signature(),
                    });
                    break;
                }
            } else {
                let parsed = self.tool_parser.process_chunk(remaining);
                if !parsed.normal_text.is_empty() {
                    let cleaned = strip_fabricated_tool_results(&parsed.normal_text);
                    if !cleaned.is_empty() {
                        content_blocks.push(ResponseContentBlock::Text { text: cleaned });
                    }
                }
                for delta in parsed.tool_deltas {
                    if let Some(func) = &delta.function {
                        let input: serde_json::Value = func
                            .arguments
                            .as_deref()
                            .and_then(|a| serde_json::from_str(a).ok())
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                        content_blocks.push(ResponseContentBlock::ToolUse {
                            id: delta.id.clone().unwrap_or_default(),
                            name: func.name.clone().unwrap_or_default(),
                            input,
                        });
                    }
                }
                break;
            }
        }

        if content_blocks.is_empty() {
            content_blocks.push(ResponseContentBlock::Text {
                text: String::new(),
            });
        }

        let has_tool_use = content_blocks
            .iter()
            .any(|b| matches!(b, ResponseContentBlock::ToolUse { .. }));

        if has_tool_use {
            content_blocks.retain(|b| !matches!(b, ResponseContentBlock::Text { .. }));
        }

        let stop_reason = if has_tool_use { "tool_use" } else { "end_turn" };

        let usage = match &self.usage {
            Some(u) => ResponseUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            },
            None => ResponseUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
        };

        MessagesResponse {
            id: self.task_id,
            r#type: "message",
            role: "assistant",
            model: self.model,
            content: content_blocks,
            stop_reason: stop_reason.to_string(),
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join_bytes(events: Vec<Bytes>) -> String {
        events
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 模拟上游返回 tool_use 文本 → 验证 SSE 事件包含 tool_use 块
    /// 文本增量到达后立即透传输出，不做缓冲聚合。
    #[test]
    fn test_stream_converter_tool_use_detection() {
        let mut conv = AnthropicStreamConverter::new("test-model".into(), "msg_1".into());

        // 发送普通文本，立即输出 content_block_start + text_delta
        let events = conv.process_ouyi_event("Let me check that.", false);
        let _joined = join_bytes(events);

        // 发送 tool_use（触发前文刷新 + tool_use 块输出）
        let events = conv.process_ouyi_event(
            r#"<tool_use>
{"name": "get_weather", "arguments": {"city": "Beijing"}}
</tool_use>"#,
            false,
        );
        let joined = join_bytes(events);
        assert!(
            joined.contains("tool_use"),
            "应包含 tool_use 块: {}",
            joined
        );
        assert!(
            joined.contains(r#""input":{}"#),
            "tool_use 起始块应包含空 input: {}",
            joined
        );
        assert!(joined.contains("get_weather"), "应包含工具名: {}", joined);
        assert!(
            joined.contains("partial_json"),
            "应包含 input_json_delta: {}",
            joined
        );
        assert!(joined.contains("Beijing"), "应包含城市参数: {}", joined);

        assert!(conv.has_tool_use, "has_tool_use 应为 true");
        assert!(conv.last_tool_use_id.is_some(), "应记录 last_tool_use_id");

        // 发送 done 事件
        let done_events = conv.process_ouyi_event("", true);
        let done_joined = join_bytes(done_events);
        assert!(
            done_joined.contains(r#""stop_reason":"tool_use""#),
            "stop_reason 应为 tool_use: {}",
            done_joined
        );
        assert!(
            done_joined.contains("message_stop"),
            "应包含 message_stop: {}",
            done_joined
        );
    }

    /// 验证 has_tool_use 为 true 后，后续文本被忽略。
    #[test]
    fn test_stream_converter_ignores_text_after_tool_use() {
        let mut conv = AnthropicStreamConverter::new("test-model".into(), "msg_1".into());

        conv.process_ouyi_event(
            r#"<tool_use>{"name":"test","arguments":{}}</tool_use>"#,
            false,
        );
        assert!(conv.has_tool_use);

        // 后续文本应被忽略
        let events = conv.process_ouyi_event("This should be ignored", false);
        let joined = join_bytes(events);
        assert!(
            !joined.contains("text_delta"),
            "tool_use 后的文本应被忽略: {}",
            joined
        );
    }

    /// 验证纯文本响应正常结束（文本增量到达后立即输出，不做缓冲聚合）
    #[test]
    fn test_stream_converter_text_response_done() {
        let mut conv = AnthropicStreamConverter::new("test-model".into(), "msg_1".into());

        // 文本增量到达后立即输出
        let events = conv.process_ouyi_event("Hello world", false);
        let joined = join_bytes(events);
        assert!(
            joined.contains(r#""type":"content_block_start""#),
            "应立即输出 content_block_start: {}",
            joined
        );
        assert!(
            joined.contains("text_delta"),
            "应立即输出 text_delta: {}",
            joined
        );
        assert!(joined.contains("Hello world"), "应包含文本内容: {}", joined);

        let done_events = conv.process_ouyi_event("", true);
        let done_joined = join_bytes(done_events);
        assert!(
            done_joined.contains(r#""stop_reason":"end_turn""#),
            "纯文本响应 stop_reason 应为 end_turn: {}",
            done_joined
        );
        assert!(
            done_joined.contains("message_stop"),
            "应包含 message_stop: {}",
            done_joined
        );
    }

    /// 模拟完整工具调用流：首次 text → tool_use → done → 模拟 tool_result 响应
    #[test]
    fn test_stream_converter_full_tool_flow() {
        // === 轮次 1: 带 tool_use 的响应 ===
        let mut conv = AnthropicStreamConverter::new("test-model".into(), "msg_1".into());

        conv.process_ouyi_event("I'll check the weather.\n", false);
        conv.process_ouyi_event(
            r#"<tool_use>
{"name": "get_weather", "arguments": {"city": "Beijing"}}
</tool_use>"#,
            false,
        );

        assert!(conv.has_tool_use);
        assert_eq!(conv.last_tool_use_id, Some("call_0".to_string()));

        let done = conv.process_ouyi_event("", true);
        let done_str = join_bytes(done);
        assert!(done_str.contains(r#""stop_reason":"tool_use""#));

        // === 轮次 2: tool_result 后的最终文本响应 ===
        let mut conv2 = AnthropicStreamConverter::new("test-model".into(), "msg_2".into());

        // 文本立即输出
        let long_text = format!("The weather in Beijing is sunny, 25°C. {}", "x".repeat(50));
        let events = conv2.process_ouyi_event(&long_text, false);
        let _joined = join_bytes(events);

        assert!(conv2.has_content, "文本应立即输出，has_content 应为 true");

        // 发送 done 获得完整事件
        let done2 = conv2.process_ouyi_event("", true);
        let done2_str = join_bytes(done2);
        assert!(
            done2_str.contains(r#""stop_reason":"end_turn""#),
            "最终响应 stop_reason 应为 end_turn: {}",
            done2_str
        );
    }

    /// 验证 strip_fabricated_tool_results 正确清除模型自造的 tool_result
    #[test]
    fn test_strip_fabricated_tool_results_removes_tags() {
        let input = r#"Let me think about this.
<tool_result name="search">
some results
</tool_result>
Now I can answer."#;

        let cleaned = strip_fabricated_tool_results(input);
        assert!(
            !cleaned.contains("<tool_result"),
            "应移除 <tool_result 标签"
        );
        assert!(
            !cleaned.contains("</tool_result>"),
            "应移除 </tool_result> 标签"
        );
        assert!(cleaned.contains("Let me think about this"));
        assert!(cleaned.contains("Now I can answer"));
        assert!(!cleaned.contains("some results"), "应移除 tool_result 内容");
    }

    /// 验证 tool_use_id 使用合成 ID 从 ToolCallParser 正确传递
    #[test]
    fn test_tool_use_id_generation() {
        let mut parser = ToolCallParser::new();

        let output = parser.process_chunk(
            r#"<tool_use>
{"name": "search", "arguments": {"query": "rust"}}
</tool_use>"#,
        );

        assert_eq!(output.tool_deltas.len(), 1);
        let delta = &output.tool_deltas[0];
        assert_eq!(delta.index, 0);
        assert_eq!(delta.id, Some("call_0".to_string()));

        // 第二个 tool_use 应有不同 ID
        let output2 = parser.process_chunk(
            r#"<tool_use>
{"name": "read", "arguments": {"path": "/tmp"}}
</tool_use>"#,
        );

        assert_eq!(output2.tool_deltas.len(), 1);
        let delta2 = &output2.tool_deltas[0];
        assert_eq!(delta2.index, 1);
        assert_eq!(delta2.id, Some("call_1".to_string()));
    }
}
