use crate::model::ouyi::OuyiSseEvent;

#[derive(Debug, Clone, PartialEq)]
pub enum OuyiEventType {
    StringChunk,
    Object,
    Done,
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

pub fn parse_sse_line(line: &str) -> Option<OuyiSseEvent> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str::<OuyiSseEvent>(data).ok()
}

pub struct SseLineBuffer {
    buffer: String,
}

impl SseLineBuffer {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

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
}
