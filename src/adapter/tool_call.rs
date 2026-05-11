use crate::model::openai::{ToolCall, ToolCallFunction, ToolCallDelta, ToolCallFunctionDelta};

const TOOL_USE_OPEN: &str = "<tool_use>";
const TOOL_USE_CLOSE: &str = "</tool_use>";

fn eq_tag_char(a: char, b: char) -> bool {
    a == b || norm_tag_char(a) == norm_tag_char(b)
}

fn norm_tag_char(c: char) -> char {
    match c {
        '\u{FF5C}' => '|',
        '\u{2581}' => '_',
        '\u{FF1C}' => '<',
        '\u{FF1E}' => '>',
        _ => c,
    }
}

fn fuzzy_match_tag(haystack: &str, needle: &str) -> bool {
    let h_chars: Vec<char> = haystack.chars().collect();
    let n_chars: Vec<char> = needle.chars().collect();
    if h_chars.len() > n_chars.len() {
        return false;
    }
    for (i, hc) in h_chars.iter().enumerate() {
        if !eq_tag_char(*hc, n_chars[i]) {
            return false;
        }
    }
    h_chars.len() == n_chars.len()
}

fn fuzzy_prefix_match(haystack: &str, needle: &str) -> bool {
    let h_chars: Vec<char> = haystack.chars().collect();
    let n_chars: Vec<char> = needle.chars().collect();
    if h_chars.len() > n_chars.len() {
        return false;
    }
    for (i, hc) in h_chars.iter().enumerate() {
        if !eq_tag_char(*hc, n_chars[i]) {
            return false;
        }
    }
    true
}

fn is_tag_start_char(ch: char) -> bool {
    ch == '<' || ch == '\u{FF1C}'
}

fn is_tag_end_char(ch: char) -> bool {
    ch == '>' || ch == '\u{FF1E}'
}

fn strip_code_fences(s: &str) -> String {
    let mut result = s.to_string();
    for fence in &["```json", "```xml", "```"] {
        result = result.replace(fence, "");
    }
    result.trim().to_string()
}

fn repair_invalid_backslashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&next)
                    if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') =>
                {
                    out.push('\\');
                    out.push(next);
                    chars.next();
                }
                Some(&next) => {
                    out.push('\\');
                    out.push('\\');
                    out.push(next);
                    chars.next();
                }
                None => {
                    out.push('\\');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn repair_unquoted_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        if (chars[i] == '{' || chars[i] == ',') && i + 1 < len {
            out.push(chars[i]);
            i += 1;
            while i < len && chars[i].is_whitespace() {
                out.push(chars[i]);
                i += 1;
            }
            if i < len && (chars[i].is_alphabetic() || chars[i] == '_') {
                let key_start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                if i < len && chars[i] == ':' {
                    out.push('"');
                    out.extend(&chars[key_start..i]);
                    out.push('"');
                } else {
                    out.extend(&chars[key_start..i]);
                    continue;
                }
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn repair_json(s: &str) -> Option<String> {
    let step0: String = s.chars().map(|c| if c == '\'' { '"' } else { c }).collect();
    if step0 != s
        && serde_json::from_str::<serde_json::Value>(&step0).is_ok()
    {
        return Some(step0);
    }
    let step1 = repair_invalid_backslashes(s);
    if serde_json::from_str::<serde_json::Value>(&step1).is_ok() {
        return Some(step1);
    }
    let step2 = repair_unquoted_keys(&step1);
    if serde_json::from_str::<serde_json::Value>(&step2).is_ok() {
        return Some(step2);
    }
    None
}

pub struct ToolCallParser {
    state: ParseState,
    json_buf: String,
    tag_buf: String,
    tool_call_index: u32,
    pub in_tool: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum ParseState {
    Normal,
    TagDetect,
    InToolJson,
    CloseTagDetect,
}

impl ToolCallParser {
    pub fn new() -> Self {
        Self {
            state: ParseState::Normal,
            json_buf: String::new(),
            tag_buf: String::new(),
            tool_call_index: 0,
            in_tool: false,
        }
    }

    pub fn process_chunk(&mut self, content: &str) -> ParsedOutput {
        let mut normal_text = String::new();
        let mut tool_deltas: Vec<ToolCallDelta> = Vec::new();

        for ch in content.chars() {
            match self.state {
                ParseState::Normal => {
                    if is_tag_start_char(ch) {
                        self.tag_buf.clear();
                        self.tag_buf.push(ch);
                        self.state = ParseState::TagDetect;
                    } else {
                        normal_text.push(ch);
                    }
                }
                ParseState::TagDetect => {
                    self.tag_buf.push(ch);
                    let tag = &self.tag_buf;
                    if fuzzy_match_tag(tag, TOOL_USE_OPEN) {
                        self.json_buf.clear();
                        self.in_tool = true;
                        self.state = ParseState::InToolJson;
                    } else if is_tag_end_char(ch) || !could_match_tag_fuzzy(tag) {
                        normal_text.push_str(tag);
                        self.state = ParseState::Normal;
                    }
                }
                ParseState::InToolJson => {
                    if is_tag_start_char(ch) {
                        self.tag_buf.clear();
                        self.tag_buf.push(ch);
                        self.state = ParseState::CloseTagDetect;
                    } else {
                        self.json_buf.push(ch);
                    }
                }
                ParseState::CloseTagDetect => {
                    self.tag_buf.push(ch);
                    if fuzzy_match_tag(&self.tag_buf, TOOL_USE_CLOSE) {
                        let json_content = std::mem::take(&mut self.json_buf);
                        if let Some(delta) = self.parse_tool_json_repair(&json_content) {
                            tool_deltas.push(delta);
                        }
                        self.in_tool = false;
                        self.state = ParseState::Normal;
                    } else if is_tag_end_char(ch) || !could_match_close_tag_fuzzy(&self.tag_buf) {
                        self.json_buf.push_str(&self.tag_buf);
                        self.state = ParseState::InToolJson;
                    }
                }
            }
        }

        ParsedOutput {
            normal_text,
            tool_deltas,
        }
    }

    fn parse_tool_json_repair(&mut self, raw: &str) -> Option<ToolCallDelta> {
        let trimmed = raw.trim();

        // Layer 1: 直接 JSON 解析
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
            && let Some(tc) = self.json_to_tool_call_delta(&parsed)
        {
            return Some(tc);
        }

        // Layer 2: 文本修复（剥离代码块 → 尝试解析）
        let cleaned = strip_code_fences(trimmed);
        if cleaned != trimmed {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&cleaned)
                && let Some(tc) = self.json_to_tool_call_delta(&parsed)
            {
                return Some(tc);
            }
        }

        // Layer 3: JSON 修复（反斜杠修复 → 无引号 key 修复）
        if let Some(repaired) = repair_json(&cleaned) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&repaired)
                && let Some(tc) = self.json_to_tool_call_delta(&parsed)
            {
                return Some(tc);
            }
        }
        if let Some(repaired) = repair_json(trimmed) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&repaired)
                && let Some(tc) = self.json_to_tool_call_delta(&parsed)
            {
                return Some(tc);
            }
        }

        // Layer 4: 字段提取（兼容格式不完整的 JSON）
        if let Some(tc) = self.extract_tool_call(&cleaned) {
            return Some(tc);
        }
        if let Some(tc) = self.extract_tool_call(trimmed) {
            return Some(tc);
        }

        self.in_tool = false;
        tracing::warn!(raw_len = trimmed.len(), "tool call 四层解析全部失败，降级为普通文本");
        None
    }

    fn json_to_tool_call_delta(&mut self, v: &serde_json::Value) -> Option<ToolCallDelta> {
        let name = v.get("name")?.as_str()?.to_string();
        let arguments = v
            .get("arguments")
            .and_then(|a| serde_json::to_string(a).ok())
            .unwrap_or_else(|| "{}".to_string());

        let idx = self.tool_call_index;
        self.tool_call_index += 1;

        Some(ToolCallDelta {
            index: idx,
            id: Some(format!("call_{}", idx)),
            call_type: Some("function".to_string()),
            function: Some(ToolCallFunctionDelta {
                name: Some(name),
                arguments: Some(arguments),
            }),
        })
    }

    fn extract_tool_call(&mut self, raw: &str) -> Option<ToolCallDelta> {
        let name = extract_json_string_field(raw, "name")?;
        let arguments =
            extract_json_string_field(raw, "arguments").unwrap_or_else(|| "{}".to_string());

        let idx = self.tool_call_index;
        self.tool_call_index += 1;

        Some(ToolCallDelta {
            index: idx,
            id: Some(format!("call_{}", idx)),
            call_type: Some("function".to_string()),
            function: Some(ToolCallFunctionDelta {
                name: Some(name),
                arguments: Some(arguments),
            }),
        })
    }

    pub fn is_in_tool_call(&self) -> bool {
        self.in_tool
    }
}

fn could_match_tag_fuzzy(buf: &str) -> bool {
    fuzzy_prefix_match(buf, TOOL_USE_OPEN)
}

fn could_match_close_tag_fuzzy(buf: &str) -> bool {
    fuzzy_prefix_match(buf, TOOL_USE_CLOSE)
}

fn extract_json_string_field(raw: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\"", field);
    let start = raw.find(&pattern)?;
    let after_key = &raw[start + pattern.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();

    if let Some(rest) = after_colon.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return Some(rest[..end].to_string());
    }

    let first_char = after_colon.chars().next()?;
    let (open, close) = match first_char {
        '{' => ('{', '}'),
        '[' => ('[', ']'),
        _ => return None,
    };
    let mut depth = 0u32;
    for (i, ch) in after_colon.char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.checked_sub(1)?;
        }
        if depth == 0 && i > 0 {
            return Some(after_colon[..=i].to_string());
        }
    }
    None
}

pub fn tool_delta_to_full_call(delta: &ToolCallDelta) -> ToolCall {
    ToolCall {
        id: delta.id.clone().unwrap_or_default(),
        call_type: "function".to_string(),
        function: ToolCallFunction {
            name: delta
                .function
                .as_ref()
                .and_then(|f| f.name.clone())
                .unwrap_or_default(),
            arguments: delta
                .function
                .as_ref()
                .and_then(|f| f.arguments.clone())
                .unwrap_or_default(),
        },
    }
}

#[derive(Debug)]
pub struct ParsedOutput {
    pub normal_text: String,
    pub tool_deltas: Vec<ToolCallDelta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_text_passthrough() {
        let mut parser = ToolCallParser::new();
        let out = parser.process_chunk("Hello world");
        assert_eq!(out.normal_text, "Hello world");
        assert!(out.tool_deltas.is_empty());
    }

    #[test]
    fn test_tool_call_parsing() {
        let mut parser = ToolCallParser::new();
        let input = r#"Some text <tool_use>
{"name": "read_file", "arguments": {"path": "/tmp/test"}}
</tool_use> more text"#;
        let out = parser.process_chunk(input);
        assert!(out.normal_text.contains("Some text "));
        assert!(out.normal_text.contains(" more text"));
        assert_eq!(out.tool_deltas.len(), 1);
        assert_eq!(
            out.tool_deltas[0].function.as_ref().unwrap().name.as_deref(),
            Some("read_file")
        );
    }

    #[test]
    fn test_multiple_tool_calls() {
        let mut parser = ToolCallParser::new();
        let input = r#"<tool_use>
{"name": "tool1", "arguments": {}}
</tool_use><tool_use>
{"name": "tool2", "arguments": {}}
</tool_use>"#;
        let out = parser.process_chunk(input);
        assert_eq!(out.tool_deltas.len(), 2);
        assert_eq!(out.tool_deltas[0].index, 0);
        assert_eq!(out.tool_deltas[1].index, 1);
    }

    #[test]
    fn test_incomplete_tag_rollback() {
        let mut parser = ToolCallParser::new();
        let out = parser.process_chunk("text <div>more");
        assert!(out.normal_text.contains("text "));
        assert!(out.normal_text.contains("<div>"));
        assert!(out.tool_deltas.is_empty());
    }

    #[test]
    fn test_malformed_json_layer2() {
        let mut parser = ToolCallParser::new();
        let input = r#"<tool_use>
{"name": "broken_tool", "arguments": {invalid json}}
</tool_use>"#;
        let out = parser.process_chunk(input);
        assert_eq!(out.tool_deltas.len(), 1);
        assert_eq!(
            out.tool_deltas[0].function.as_ref().unwrap().name.as_deref(),
            Some("broken_tool")
        );
    }

    #[test]
    fn test_extract_json_string_field() {
        let raw = r#"{"name": "read_file", "arguments": {"path": "/tmp"}}"#;
        assert_eq!(
            extract_json_string_field(raw, "name"),
            Some("read_file".to_string())
        );
    }

    #[test]
    fn test_fullwidth_tag_parsing() {
        let mut parser = ToolCallParser::new();
        let input = "Some text \u{ff1c}tool_use\u{ff1e}\n{\"name\": \"read\", \"arguments\": {}}\n\u{ff1c}/tool_use\u{ff1e} after";
        let out = parser.process_chunk(input);
        assert_eq!(out.tool_deltas.len(), 1);
        assert_eq!(
            out.tool_deltas[0].function.as_ref().unwrap().name.as_deref(),
            Some("read")
        );
    }

    #[test]
    fn test_code_fence_removal() {
        let mut parser = ToolCallParser::new();
        let input = r#"<tool_use>
```json
{"name": "bash", "arguments": {"command": "ls"}}
```
</tool_use>"#;
        let out = parser.process_chunk(input);
        assert_eq!(out.tool_deltas.len(), 1);
        assert_eq!(
            out.tool_deltas[0].function.as_ref().unwrap().name.as_deref(),
            Some("bash")
        );
    }

    #[test]
    fn test_unquoted_keys_repair() {
        let mut parser = ToolCallParser::new();
        let input = r#"<tool_use>
{name: "read", arguments: {path: "/tmp"}}
</tool_use>"#;
        let out = parser.process_chunk(input);
        assert_eq!(out.tool_deltas.len(), 1);
        assert_eq!(
            out.tool_deltas[0].function.as_ref().unwrap().name.as_deref(),
            Some("read")
        );
    }

    #[test]
    fn test_single_quotes_repair() {
        let parsed = repair_json("{'name': 'read', 'arguments': {}}");
        assert!(parsed.is_some());
    }

    #[test]
    fn test_underscore_tag_variant() {
        let mut parser = ToolCallParser::new();
        let input = "text <tool\u{2581}use>\n{\"name\": \"read\", \"arguments\": {}}\n</tool\u{2581}use> more";
        let out = parser.process_chunk(input);
        assert_eq!(out.tool_deltas.len(), 1);
        assert_eq!(
            out.tool_deltas[0].function.as_ref().unwrap().name.as_deref(),
            Some("read")
        );
    }

    #[test]
    fn test_fuzzy_tag_match() {
        assert!(fuzzy_match_tag("<tool_use>", "<tool_use>"));
        assert!(fuzzy_match_tag("\u{ff1c}tool_use\u{ff1e}", "<tool_use>"));
        assert!(!fuzzy_match_tag("<div>", "<tool_use>"));
        assert!(!fuzzy_match_tag("<tool_", "<tool_use>"));  // 部分匹配返回 false
        assert!(fuzzy_prefix_match("<tool_", "<tool_use>"));  // 前缀匹配返回 true
        assert!(!fuzzy_prefix_match("<div", "<tool_use>"));
    }

    #[test]
    fn test_strip_code_fences() {
        let input = "```json\n{\"name\": \"test\"}\n```";
        let cleaned = strip_code_fences(input);
        assert_eq!(cleaned, "{\"name\": \"test\"}");

        let input2 = "```xml\n<tool_use>\n{...}\n</tool_use>\n```";
        let cleaned2 = strip_code_fences(input2);
        assert!(!cleaned2.contains("```"));
    }
}
