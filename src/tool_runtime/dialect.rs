//! 工具调用标记方言识别与解析。
//! 支持四类方言：canonical（`<tool_call>` 通用 JSON 形式）、Anthropic
//! （`<tool_use>` 形式）、GLM 紧凑形式（`<tool_call>名▁参数▁params▁JSON`）
//! 与 call_tool 函数式语法（`call_tool Name(key=value)`）；并提供显式工具
//! 意图检测，供 broker 判断"输出了工具痕迹但未形成合法调用"的情形。

use crate::adapter::tool_call::ToolCallParser;

const GLM_OPEN: &str = "<tool_call>";
const GLM_PARAMS: &str = "▁params▁";
const CALL_TOOL_PREFIX: &str = "call_tool ";

/// 工具调用标记的方言类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDialect {
    /// 通用 JSON 形式（canonical 标记格式）。
    Canonical,
    /// Anthropic 风格的 `<tool_use>` 形式。
    Anthropic,
    /// GLM 紧凑形式（`▁params▁` 分隔的头部 + JSON 参数）。
    GlmCompact,
    /// `call_tool Name(...)` 函数式语法。
    CallTool,
}

/// 从文本中解析出的单个工具调用（未做工具名 / schema 校验）。
#[derive(Debug, Clone, PartialEq)]
pub struct DialectToolCall {
    /// 工具名称。
    pub name: String,
    /// 工具入参（已解析为 JSON 对象）。
    pub input: serde_json::Value,
    /// 该调用所属的方言。
    pub dialect: ToolDialect,
}

/// 增量缓冲解析器：分片接收流式文本，结束时统一解析。
/// 仅供测试使用（`#[cfg(test)]` 门控）。
#[derive(Debug, Default)]
#[cfg(test)]
pub struct DialectParser {
    /// 已接收的文本缓冲。
    buffer: String,
}

#[cfg(test)]
impl DialectParser {
    /// 创建空的解析器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一段流式文本到缓冲。
    ///
    /// # 参数
    /// - `chunk`：本次到达的文本片段。
    pub fn push_chunk(&mut self, chunk: &str) {
        self.buffer.push_str(chunk);
    }

    /// 结束收集并解析缓冲中的全部工具调用。
    ///
    /// # 返回
    /// 解析出的工具调用列表。
    ///
    /// # 错误
    /// 解析失败时返回描述性错误字符串（同 [`parse_known_tool_calls`]）。
    pub fn finish(self) -> Result<Vec<DialectToolCall>, String> {
        parse_known_tool_calls(&self.buffer)
    }
}

/// 检测文本中是否出现显式的工具调用标记痕迹。
///
/// 命中任一已知标记（GLM 开标记、`<tool_use>`、canonical begin 标记、
/// `call_tool ` 前缀）即视为有意图；纯自然语言提及工具不会命中。
///
/// # 参数
/// - `text`：待检测文本。
///
/// # 返回
/// `true` 表示存在显式工具调用痕迹。
pub fn has_explicit_tool_intent(text: &str) -> bool {
    text.contains(GLM_OPEN)
        || text.contains("<tool_use>")
        || text.contains("<|tool▁calls▁begin|>")
        || text.contains("<|tool_calls_begin|>")
        || text.contains(CALL_TOOL_PREFIX)
}

/// 解析文本中的全部工具调用，按方言优先级依次尝试。
///
/// 尝试顺序：GLM 紧凑形式 → call_tool 函数式语法 → canonical / Anthropic
/// 通用解析器；前一类命中即返回，不继续尝试。
///
/// # 参数
/// - `text`：待解析文本。
///
/// # 返回
/// 解析出的工具调用列表（含方言标记）；无任何标记时为空列表。
///
/// # 错误
/// 标记内部结构不完整（缺少工具名 / params 标记 / 括号未闭合）、
/// 参数不是合法 JSON 等情形返回描述性错误字符串。
pub fn parse_known_tool_calls(text: &str) -> Result<Vec<DialectToolCall>, String> {
    let glm_calls = parse_glm_calls(text)?;
    if !glm_calls.is_empty() {
        return Ok(glm_calls);
    }

    let call_tool_calls = parse_call_tool_calls(text)?;
    if !call_tool_calls.is_empty() {
        return Ok(call_tool_calls);
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

fn parse_call_tool_calls(text: &str) -> Result<Vec<DialectToolCall>, String> {
    let mut calls = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find(CALL_TOOL_PREFIX) {
        let call_start = cursor + relative_start + CALL_TOOL_PREFIX.len();
        let rest = text[call_start..].trim_start();
        let name_len = rest
            .char_indices()
            .find(|(_, ch)| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.'))
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let name = rest[..name_len].trim();
        if name.is_empty() {
            return Err("call_tool 缺少工具名".to_string());
        }
        let after_name = rest[name_len..].trim_start();
        if !after_name.starts_with('(') {
            return Err("call_tool 工具名后缺少参数括号".to_string());
        }
        let close = find_matching_parenthesis(after_name)
            .ok_or_else(|| "call_tool 参数括号未闭合".to_string())?;
        let input = parse_call_tool_arguments(&after_name[1..close])?;
        calls.push(DialectToolCall {
            name: name.to_string(),
            input,
            dialect: ToolDialect::CallTool,
        });
        cursor = text.len() - after_name.len() + close + 1;
    }
    Ok(calls)
}

fn find_matching_parenthesis(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn parse_call_tool_arguments(text: &str) -> Result<serde_json::Value, String> {
    let mut object = serde_json::Map::new();
    for part in split_call_tool_arguments(text) {
        let (key, raw_value) = part
            .split_once('=')
            .ok_or_else(|| format!("call_tool 参数缺少等号: {part}"))?;
        let key = key.trim();
        if key.is_empty() {
            return Err("call_tool 参数名为空".to_string());
        }
        let raw_value = raw_value.trim();
        let value = serde_json::from_str(raw_value).unwrap_or_else(|_| {
            serde_json::Value::String(raw_value.trim_matches('"').trim_matches('\'').to_string())
        });
        object.insert(key.to_string(), value);
    }
    Ok(serde_json::Value::Object(object))
}

fn split_call_tool_arguments(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut nested = 0usize;
    for (index, ch) in text.char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' | '{' => nested += 1,
            ')' | ']' | '}' => nested = nested.saturating_sub(1),
            ',' if nested == 0 => {
                let part = text[start..index].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
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
    fn parses_call_tool_function_syntax() {
        let calls =
            parse_known_tool_calls("I will inspect it now.\n\ncall_tool Read(file_path=\"alpha\")")
                .unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].input["file_path"], "alpha");
        assert_eq!(calls[0].dialect, ToolDialect::CallTool);
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
        assert!(has_explicit_tool_intent("call_tool Read(file_path=\"a\")"));
        assert!(!has_explicit_tool_intent("Use the Bash tool if needed."));
    }
}
