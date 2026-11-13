//! SSE 流式解析辅助：事件行解析、跨网络分块的 UTF-8 增量解码、行缓冲，
//! 以及上游应用层错误的识别与敏感信息脱敏。

use crate::model::ouyi::OuyiSseEvent;

/// 上游 SSE 事件的类型归类。
#[derive(Debug, Clone, PartialEq)]
pub enum OuyiEventType {
    /// 文本分片事件（`type == "string"`）。
    StringChunk,
    /// 结构化对象事件（`type == "object"`）。
    Object,
    /// 流结束事件（`type == "done"`）。
    Done,
    /// 无法归类的其他事件类型，携带上游原始类型串。
    Unknown(String),
}

impl From<&str> for OuyiEventType {
    fn from(s: &str) -> Self {
        match s {
            "string" => OuyiEventType::StringChunk,
            "object" => OuyiEventType::Object,
            "done" => OuyiEventType::Done,
            other => OuyiEventType::Unknown(other.to_string()),
        }
    }
}

/// 解析一行 `data:` 前缀的 SSE 事件为 [`OuyiSseEvent`]。
///
/// 非 `data:` 行、空数据或 JSON 解析失败时返回 `None`。
pub fn parse_sse_line(line: &str) -> Option<OuyiSseEvent> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str::<OuyiSseEvent>(data).ok()
}

/// UTF-8 增量解码的错误类型。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Utf8StreamError {
    /// 字节流包含非法 UTF-8 序列，`offset` 为首个非法字节前的有效长度。
    #[error("UTF-8 流包含非法字节（偏移 {offset}）")]
    InvalidSequence { offset: usize },
    /// 流在多字节字符中间结束，尾部字节已丢弃。
    #[error("UTF-8 流在多字节字符中间结束")]
    IncompleteSequence,
}

/// 跨网络分块的 UTF-8 增量解码器：暂存不完整的多字节序列，
/// 待后续分块补齐后再回调输出。
#[derive(Debug, Default)]
pub struct Utf8ChunkDecoder {
    pending: Vec<u8>,
}

impl Utf8ChunkDecoder {
    /// 创建空解码器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 送入一个字节分块，把其中完整的 UTF-8 文本通过 `on_text` 回调输出。
    ///
    /// 不完整的多字节序列尾部会暂存到下一次调用。即使返回 `Err`，
    /// 出错点之前的合法文本也已先回调输出。
    /// 返回 `Err(Utf8StreamError::InvalidSequence)` 表示分块内存在非法字节。
    pub fn push_chunk<F>(&mut self, chunk: &[u8], mut on_text: F) -> Result<(), Utf8StreamError>
    where
        F: FnMut(&str),
    {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);

        match std::str::from_utf8(&bytes) {
            Ok(text) => {
                if !text.is_empty() {
                    on_text(text);
                }
                Ok(())
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    on_text(std::str::from_utf8(&bytes[..valid_up_to]).unwrap());
                }
                if error.error_len().is_some() {
                    return Err(Utf8StreamError::InvalidSequence {
                        offset: valid_up_to,
                    });
                }

                self.pending.extend_from_slice(&bytes[valid_up_to..]);
                Ok(())
            }
        }
    }

    /// 流结束时调用：暂存区已清空返回 `Ok`；仍残留不完整序列则丢弃尾部
    /// 字节并返回 `Err(Utf8StreamError::IncompleteSequence)`。
    pub fn finish(&mut self) -> Result<(), Utf8StreamError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            self.pending.clear();
            Err(Utf8StreamError::IncompleteSequence)
        }
    }
}

const MAX_OUYI_ERROR_CHARS: usize = 256;

/// 识别 Ouyi 8.0 SSE 中 HTTP 200 包裹的应用层错误。
///
/// 只返回上游提供的错误摘要与错误码；不拼接鉴权 token 或请求正文。
pub fn ouyi_application_error(event: &OuyiSseEvent) -> Option<String> {
    let code = event.code.unwrap_or_default();
    let message = event
        .err
        .as_deref()
        .map(sanitize_ouyi_error_message)
        .filter(|message| !message.is_empty());

    if code == 0 && message.is_none() {
        return None;
    }

    let message = message.unwrap_or_else(|| "上游返回应用错误".to_string());
    if code == 0 {
        Some(message)
    } else {
        Some(format!("{message} (code={code})"))
    }
}

fn sanitize_ouyi_error_message(message: &str) -> String {
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = message.to_ascii_lowercase();
    if [
        "authorization",
        "bearer",
        "token=",
        "token:",
        "access token",
        "refresh token",
        "api_key",
        "apikey",
        "x-api-key",
        "api key",
        "api-key",
        "password",
        "cookie",
        "secret",
        "credential",
        "access key",
        "private key",
        "client secret",
        "sk-",
        "jwt",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || contains_jwt(&message)
    {
        return "上游认证错误（敏感信息已隐藏）".to_string();
    }

    let mut sanitized: String = message.chars().take(MAX_OUYI_ERROR_CHARS).collect();
    if message.chars().count() > MAX_OUYI_ERROR_CHARS {
        sanitized.push('…');
    }
    sanitized
}

fn contains_jwt(message: &str) -> bool {
    message
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | '(' | ')' | ',' | ';')
        })
        .any(|candidate| candidate.starts_with("eyJ") && candidate.matches('.').count() == 2)
}

/// SSE 行缓冲：把任意切分的文本分块还原为完整行（去 `\r`、跳过空行）。
pub struct SseLineBuffer {
    buffer: String,
}

impl SseLineBuffer {
    /// 创建空缓冲。
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// 追加文本分块，返回其中已完成的行。
    pub fn push_chunk(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut complete_lines = Vec::new();

        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim_end_matches('\r').to_string();
            self.buffer.drain(..=pos);
            if !line.is_empty() {
                complete_lines.push(line);
            }
        }

        complete_lines
    }

    /// 流结束时调用：返回缓冲中未以换行结尾的残余内容（trim 后）并清空
    /// 缓冲；无残余时返回 `None`。
    pub fn drain_remaining(&mut self) -> Option<String> {
        let remaining = self.buffer.trim().to_string();
        if remaining.is_empty() {
            None
        } else {
            self.buffer.clear();
            Some(remaining)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_from_str() {
        assert_eq!(OuyiEventType::from("string"), OuyiEventType::StringChunk);
        assert_eq!(OuyiEventType::from("object"), OuyiEventType::Object);
        assert_eq!(OuyiEventType::from("done"), OuyiEventType::Done);
        assert_eq!(
            OuyiEventType::from("custom"),
            OuyiEventType::Unknown("custom".to_string())
        );
    }

    #[test]
    fn test_parse_sse_line_string_chunk() {
        let line = r#"data:{"id":"1","type":"string","data":"hello"}"#;
        let event = parse_sse_line(line).unwrap();
        assert_eq!(event.event_type, "string");
        assert!(event.data.is_some());
    }

    #[test]
    fn test_parse_sse_line_done() {
        let line = r#"data:{"id":"1","type":"done"}"#;
        let event = parse_sse_line(line).unwrap();
        assert_eq!(event.event_type, "done");
    }

    #[test]
    fn test_parse_sse_line_preserves_ouyi_application_error() {
        let line = r#"data: {"id":"quota","type":"","data":null,"err":"您的套餐余量不足，请购买后重试","code":1}"#;
        let event = parse_sse_line(line).unwrap();

        assert_eq!(event.code, Some(1));
        assert_eq!(event.err.as_deref(), Some("您的套餐余量不足，请购买后重试"));
        assert_eq!(
            ouyi_application_error(&event).as_deref(),
            Some("您的套餐余量不足，请购买后重试 (code=1)")
        );
    }

    #[test]
    fn test_parse_sse_line_accepts_application_error_without_type() {
        let event = parse_sse_line(r#"data:{"err":"quota","code":1}"#).unwrap();

        assert_eq!(event.event_type, "");
        assert_eq!(
            ouyi_application_error(&event).as_deref(),
            Some("quota (code=1)")
        );
    }

    #[test]
    fn test_ouyi_application_error_detects_nonzero_code_without_message() {
        let event = parse_sse_line(r#"data:{"id":"1","type":"","data":null,"code":7}"#).unwrap();

        assert_eq!(
            ouyi_application_error(&event).as_deref(),
            Some("上游返回应用错误 (code=7)")
        );
    }

    #[test]
    fn test_ouyi_application_error_ignores_success_event() {
        let event =
            parse_sse_line(r#"data:{"id":"1","type":"string","data":"ok","err":"  ","code":0}"#)
                .unwrap();

        assert_eq!(ouyi_application_error(&event), None);
    }

    #[test]
    fn test_ouyi_application_error_redacts_sensitive_message() {
        let event = parse_sse_line(
            r#"data:{"id":"1","type":"","data":null,"err":"Bearer secret-token-value expired","code":1}"#,
        )
        .unwrap();

        let message = ouyi_application_error(&event).unwrap();
        assert!(message.contains("敏感信息已隐藏"));
        assert!(!message.contains("secret-token-value"));
    }

    #[test]
    fn test_ouyi_application_error_redacts_all_sensitive_markers() {
        let cases = [
            ("x-api-key", "x-api-key=leak-key-123", "leak-key-123"),
            (
                "password",
                "password=leak-password-456",
                "leak-password-456",
            ),
            ("cookie", "cookie=leak-cookie-789", "leak-cookie-789"),
            ("secret", "secret=leak-value-abc", "leak-value-abc"),
            ("api key", "api key=leak-api-space", "leak-api-space"),
            ("api-key", "api-key=leak-api-hyphen", "leak-api-hyphen"),
            (
                "credential",
                "credential=leak-credential",
                "leak-credential",
            ),
            ("access key", "access key=leak-access", "leak-access"),
            ("private key", "private key=leak-private", "leak-private"),
            ("client secret", "client secret=leak-client", "leak-client"),
            ("sk-", "upstream key sk-leak-bare", "sk-leak-bare"),
            (
                "jwt-shape",
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.signature-leak",
                "signature-leak",
            ),
        ];

        for (marker, upstream_message, sensitive_value) in cases {
            let line = format!(
                "data: {}",
                serde_json::json!({
                    "id": marker,
                    "type": "",
                    "data": null,
                    "err": upstream_message,
                    "code": 1
                })
            );
            let event = parse_sse_line(&line).unwrap();
            let message = ouyi_application_error(&event).unwrap();

            assert!(message.contains("敏感信息已隐藏"), "marker={marker}");
            assert!(!message.contains(sensitive_value), "marker={marker}");
        }
    }

    #[test]
    fn test_ouyi_application_error_keeps_token_limit_message() {
        let event = parse_sse_line(
            r#"data:{"id":"1","type":"","data":null,"err":"maximum token limit exceeded","code":1}"#,
        )
        .unwrap();

        assert_eq!(
            ouyi_application_error(&event).as_deref(),
            Some("maximum token limit exceeded (code=1)")
        );
    }

    #[test]
    fn test_parse_sse_line_invalid() {
        assert!(parse_sse_line("not sse data").is_none());
        assert!(parse_sse_line("data:").is_none());
        assert!(parse_sse_line("data:  ").is_none());
    }

    #[test]
    fn test_sse_line_buffer_single_line() {
        let mut buf = SseLineBuffer::new();
        let lines = buf.push_chunk("data:hello\n");
        assert_eq!(lines, vec!["data:hello"]);
    }

    #[test]
    fn test_sse_line_buffer_multiple_lines() {
        let mut buf = SseLineBuffer::new();
        let lines = buf.push_chunk("data:line1\ndata:line2\n");
        assert_eq!(lines, vec!["data:line1", "data:line2"]);
    }

    #[test]
    fn test_utf8_chunk_decoder_emits_quota_before_incomplete_eof_error() {
        let quota = concat!(
            "data: {\"id\":\"quota\",\"type\":\"\",\"data\":null,",
            "\"err\":\"您的套餐余量不足，请购买后重试\",\"code\":1}\n\n"
        );
        let mut chunk = quota.as_bytes().to_vec();
        chunk.extend_from_slice(&[0xE5, 0xA5]);
        let mut decoder = Utf8ChunkDecoder::new();
        let mut decoded = String::new();

        decoder
            .push_chunk(&chunk, |text| decoded.push_str(text))
            .unwrap();

        let event = decoded.lines().find_map(parse_sse_line).unwrap();
        assert_eq!(
            ouyi_application_error(&event).as_deref(),
            Some("您的套餐余量不足，请购买后重试 (code=1)")
        );
        assert_eq!(decoder.finish(), Err(Utf8StreamError::IncompleteSequence));
    }

    #[test]
    fn test_utf8_chunk_decoder_emits_valid_prefix_before_invalid_byte_error() {
        let mut decoder = Utf8ChunkDecoder::new();
        let mut decoded = String::new();

        let error = decoder
            .push_chunk(b"data: valid\n\xff", |text| decoded.push_str(text))
            .unwrap_err();

        assert_eq!(decoded, "data: valid\n");
        assert!(matches!(error, Utf8StreamError::InvalidSequence { .. }));
    }
}
