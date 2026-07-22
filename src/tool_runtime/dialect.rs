use crate::adapter::tool_call::ToolCallParser;

const GLM_OPEN: &str = "<tool_call>";
const GLM_PARAMS: &str = "▁params▁";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDialect {
    Canonical,
    Anthropic,
    GlmCompact,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DialectToolCall {
    pub name: String,
    pub input: serde_json::Value,
    pub dialect: ToolDialect,
}

#[derive(Debug, Default)]
#[cfg(test)]
pub struct DialectParser {
    buffer: String,
}

#[cfg(test)]
impl DialectParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_chunk(&mut self, chunk: &str) {
        self.buffer.push_str(chunk);
    }

    pub fn finish(self) -> Result<Vec<DialectToolCall>, String> {
        parse_known_tool_calls(&self.buffer)
    }
}

pub fn has_explicit_tool_intent(text: &str) -> bool {
    text.contains(GLM_OPEN)
        || text.contains("<tool_use>")
        || text.contains("<|tool▁calls▁begin|>")
        || text.contains("<|tool_calls_begin|>")
}

pub fn parse_known_tool_calls(text: &str) -> Result<Vec<DialectToolCall>, String> {
    let glm_calls = parse_glm_calls(text)?;
    if !glm_calls.is_empty() {
        return Ok(glm_calls);
    }

    let dialect = if text.contains("<tool_use>") {
        ToolDialect::Anthropic
    } else {
        ToolDialect::Canonical
    };
    let mut parser = ToolCallParser::new();
    let parsed = parser.process_chunk(text);
    parsed
        .tool_deltas
        .into_iter()
        .map(|delta| {
            let function = delta
                .function
                .ok_or_else(|| "工具调用缺少 function".to_string())?;
            let name = function
                .name
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "工具调用缺少 name".to_string())?;
            let arguments = function.arguments.unwrap_or_else(|| "{}".to_string());
            let input = serde_json::from_str(&arguments)
                .map_err(|error| format!("工具参数 JSON 无效: {error}"))?;
            Ok(DialectToolCall {
                name,
                input,
                dialect: dialect.clone(),
            })
        })
        .collect()
}

fn parse_glm_calls(text: &str) -> Result<Vec<DialectToolCall>, String> {
    let mut calls = Vec::new();
    let mut cursor = 0;

    while let Some(relative_start) = text[cursor..].find(GLM_OPEN) {
        let body_start = cursor + relative_start + GLM_OPEN.len();
        let body = &text[body_start..];
        let params_offset = body
            .find(GLM_PARAMS)
            .ok_or_else(|| "GLM 工具调用缺少 params 标记".to_string())?;
        let header = &body[..params_offset];
        let name = header
            .split('▁')
            .next()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "GLM 工具调用缺少工具名".to_string())?;
        let json_start = body_start + params_offset + GLM_PARAMS.len();
        let (input, consumed) = parse_first_json_value(&text[json_start..])?;
        calls.push(DialectToolCall {
            name: name.to_string(),
            input,
            dialect: ToolDialect::GlmCompact,
        });
        cursor = json_start + consumed;
    }

    Ok(calls)
}

fn parse_first_json_value(text: &str) -> Result<(serde_json::Value, usize), String> {
    let leading = text.len() - text.trim_start().len();
    let trimmed = &text[leading..];
    let first = trimmed
        .chars()
        .next()
        .ok_or_else(|| "GLM 工具调用缺少参数 JSON".to_string())?;
    let closing = match first {
        '{' => '}',
        '[' => ']',
        _ => return Err("GLM 工具参数必须是 JSON object 或 array".to_string()),
    };

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in trimmed.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == first {
            depth += 1;
        } else if ch == closing {
            depth = depth
                .checked_sub(1)
                .ok_or_else(|| "GLM 工具参数括号不匹配".to_string())?;
            if depth == 0 {
                let end = offset + ch.len_utf8();
                let raw = &trimmed[..end];
                let value = serde_json::from_str(raw)
                    .map_err(|error| format!("GLM 工具参数 JSON 无效: {error}"))?;
                return Ok((value, leading + end));
            }
        }
    }

    Err("GLM 工具参数 JSON 未闭合".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_glm_compact_tool_call() {
        let text = r#"<tool_call>Bash▁command▁params▁{"command":"pwd"}"#;
        let calls = parse_known_tool_calls(text).unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "pwd");
        assert_eq!(calls[0].dialect, ToolDialect::GlmCompact);
    }

    #[test]
    fn parses_glm_read_and_grep_without_hardcoded_names() {
        let text = concat!(
            "<tool_call>Read▁file_path▁params▁{\"file_path\":\"src/main.rs\"}\n",
            "<tool_call>Grep▁pattern▁params▁{\"pattern\":\"TODO\",\"path\":\"src\"}"
        );
        let calls = parse_known_tool_calls(text).unwrap();

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[1].name, "Grep");
        assert_eq!(calls[1].input["path"], "src");
    }

    #[test]
    fn buffers_glm_tag_and_json_across_chunks() {
        let mut parser = DialectParser::new();
        parser.push_chunk("<tool_");
        parser.push_chunk("call>Bash▁command▁params▁{\"command\":");
        parser.push_chunk("\"pwd\"}");

        let calls = parser.finish().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].input["command"], "pwd");
    }

    #[test]
    fn recognizes_explicit_intent_without_misclassifying_plain_text() {
        assert!(has_explicit_tool_intent("<tool_call>Bash▁params▁{}"));
        assert!(has_explicit_tool_intent("<tool_use>{}</tool_use>"));
        assert!(!has_explicit_tool_intent("Use the Bash tool if needed."));
    }
}
