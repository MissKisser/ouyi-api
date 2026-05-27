use crate::adapter::tool_call::{repair_json, ToolCallParser};
use crate::config::ToolRuntimeConfig;
use crate::error::AppError;
use crate::model::anthropic::MessagesRequest;
use crate::tool_runtime::protocol::ToolSpec;

fn broker_id() -> String {
    format!("call_broker_{}", uuid::Uuid::new_v4().simple())
}

fn parse_arguments_json(arguments: &str, tool_name: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(arguments) {
        return Some(v);
    }
    let repaired = repair_json(arguments)?;
    match serde_json::from_str(&repaired) {
        Ok(v) => {
            tracing::info!(%tool_name, "工具参数 JSON 已自动修复");
            Some(v)
        }
        Err(e) => {
            tracing::warn!(
                %tool_name,
                arguments_len = arguments.len(),
                %e,
                "工具参数 JSON 解析失败，跳过该工具调用"
            );
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrokerUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone)]
pub enum BrokerDecision {
    ToolCalls(Vec<BrokerToolCall>),
    FinalAnswer { usage: Option<BrokerUsage> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

pub fn find_tool<'a>(tools: &'a [ToolSpec], name: &str) -> Option<&'a ToolSpec> {
    tools.iter().find(|tool| tool.name == name)
}

pub fn parse_marked_tool_calls(
    ouyi_output: &str,
    tools: &[ToolSpec],
) -> Result<Vec<BrokerToolCall>, String> {
    let mut parser = ToolCallParser::new();
    let parsed = parser.process_chunk(ouyi_output);
    let mut calls = Vec::new();

    for delta in parsed.tool_deltas {
        let func = delta
            .function
            .ok_or_else(|| "工具调用缺少 function".to_string())?;
        let name = func
            .name
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "工具调用缺少 name".to_string())?;
        if find_tool(tools, &name).is_none() {
            tracing::warn!(%name, "主模型输出未知工具，已跳过");
            continue;
        }
        let arguments = func.arguments.unwrap_or_else(|| "{}".to_string());
        let input = match parse_arguments_json(&arguments, &name) {
            Some(v) => v,
            None => continue,
        };
        calls.push(BrokerToolCall {
            id: broker_id(),
            name,
            input,
        });
    }

    Ok(calls)
}

/// 从文本中提取代码块或单行命令。
/// 支持 ``` 包裹的命令（可带语言标记），或单行纯文本命令。
/// 多行自然语言会被拒绝。
fn extract_explicit_command(raw: &str) -> Option<String> {
    let text = raw.trim();
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        let after = after
            .strip_prefix("powershell\n")
            .or_else(|| after.strip_prefix("bash\n"))
            .or_else(|| after.strip_prefix("sh\n"))
            .or_else(|| after.strip_prefix("powershell\r\n"))
            .or_else(|| after.strip_prefix("bash\r\n"))
            .or_else(|| after.strip_prefix("sh\r\n"))
            .unwrap_or(after);
        let end = after.find("```")?;
        let command = after[..end].trim();
        if !command.is_empty() {
            return Some(command.to_string());
        }
    }
    if text.contains('\n') || text.contains('\r') {
        return None;
    }
    let command = text.trim_matches('`').trim();
    if command.is_empty() {
        None
    } else {
        Some(command.to_string())
    }
}

pub fn parse_rule_tool_intent(
    ouyi_output: &str,
    tools: &[ToolSpec],
) -> Result<Vec<BrokerToolCall>, String> {
    let text = ouyi_output.trim();

    if find_tool(tools, "Bash").is_some() {
        for marker in [
            "执行命令：",
            "执行命令:",
            "Bash 执行命令：",
            "Bash 执行命令:",
        ] {
            if let Some((_, after_marker)) = text.rsplit_once(marker) {
                if let Some(command) = extract_explicit_command(after_marker) {
                    return Ok(vec![BrokerToolCall {
                        id: broker_id(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({
                            "command": command,
                            "description": "执行欧亿主模型请求的命令"
                        }),
                    }]);
                }
            }
        }
    }

    if find_tool(tools, "Read").is_some() {
        for marker in ["读取文件 ", "读取文件：", "读取文件:", "Read 文件 "] {
            if let Some((_, path)) = text.rsplit_once(marker) {
                let path = path.trim().trim_matches('`').trim();
                if !path.is_empty() && !path.contains('\n') {
                    return Ok(vec![BrokerToolCall {
                        id: broker_id(),
                        name: "Read".to_string(),
                        input: serde_json::json!({ "file_path": path }),
                    }]);
                }
            }
        }
    }

    Ok(Vec::new())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongTaskPlan {
    pub file_path: String,
    pub done_marker: String,
    pub interval_secs: u64,
    pub max_polls: u32,
}

/// 从 echo > 重定向模式中提取文件路径和完成标记。
/// 匹配类似 `echo DONE > file.txt` 的命令。
fn detect_echo_redirect(command: &str) -> Option<(String, String)> {
    let file_path = command
        .split('>')
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .map(|s| s.trim_matches('"').trim_matches('\'').to_string())?;
    let done_marker = command
        .split("echo")
        .nth(1)
        .and_then(|tail| tail.split('>').next())
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())?;
    Some((file_path, done_marker))
}

/// 从 PowerShell Set-Content 模式中提取文件路径和完成标记。
/// 匹配类似 `Set-Content -LiteralPath file.txt -Value MARKER` 的命令。
fn detect_powershell_set_content(command: &str) -> Option<(String, String)> {
    let ps_start = command.find("Set-Content")?;
    let after = &command[ps_start..];

    let path = after
        .split("-LiteralPath")
        .nth(1)
        .or_else(|| after.split("-Path").nth(1))
        .and_then(|tail| {
            let trimmed = tail.trim();
            let end = trimmed.find(" -").unwrap_or(trimmed.len());
            Some(
                trimmed[..end]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            )
        })?;

    let marker = after.split("-Value").nth(1).and_then(|tail| {
        let trimmed = tail.trim();
        let end = trimmed
            .find(" -")
            .unwrap_or(trimmed.find(';').unwrap_or(trimmed.len()));
        Some(
            trimmed[..end]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        )
    })?;

    if !path.is_empty() && !marker.is_empty() {
        Some((path, marker))
    } else {
        None
    }
}

pub fn detect_long_task(call: &BrokerToolCall) -> Option<LongTaskPlan> {
    if call.name != "Bash" {
        return None;
    }
    let command = call.input.get("command")?.as_str()?;
    let has_long_signal = command.contains("sleep ")
        || command.contains("Start-Sleep")
        || command.contains("run_in_background")
        || command.contains("后台");
    if !has_long_signal {
        return None;
    }

    if let Some((file_path, done_marker)) = detect_powershell_set_content(command) {
        return Some(LongTaskPlan {
            file_path,
            done_marker,
            interval_secs: 5,
            max_polls: 8,
        });
    }

    if let Some((file_path, done_marker)) = detect_echo_redirect(command) {
        return Some(LongTaskPlan {
            file_path,
            done_marker,
            interval_secs: 5,
            max_polls: 8,
        });
    }

    None
}

pub fn build_poll_call(plan: &LongTaskPlan, tools: &[ToolSpec]) -> Result<BrokerToolCall, String> {
    if find_tool(tools, "Bash").is_none() {
        return Err("长时任务轮询需要 Bash 工具".to_string());
    }
    let escaped_path = plan.file_path.replace('\'', "'\\''");
    let marker = plan.done_marker.replace('\'', "'\\''");
    let command = format!(
        "sleep {}; if test -f '{}' && grep -q '{}' '{}'; then cat '{}'; else echo PENDING; fi",
        plan.interval_secs, escaped_path, marker, escaped_path, escaped_path
    );
    Ok(BrokerToolCall {
        id: broker_id(),
        name: "Bash".to_string(),
        input: serde_json::json!({
            "command": command,
            "description": format!("检查长时任务是否完成: {}", marker)
        }),
    })
}

pub enum LongTaskResult {
    Pending,
    Done,
    Failed(String),
}

pub fn classify_long_task_result(content: &str, done_marker: &str) -> LongTaskResult {
    if content.contains(done_marker) {
        LongTaskResult::Done
    } else if let Some(message) = content.trim().strip_prefix("FAILED:") {
        LongTaskResult::Failed(message.trim().to_string())
    } else if content.contains("PENDING") || content.trim().is_empty() {
        LongTaskResult::Pending
    } else {
        LongTaskResult::Pending
    }
}

pub async fn decide_with_broker(
    http_client: reqwest::Client,
    tool_runtime_config: Option<&ToolRuntimeConfig>,
    req: &MessagesRequest,
    ouyi_output: &str,
) -> Result<BrokerDecision, AppError> {
    let tool_specs: Vec<ToolSpec> = req
        .tools
        .as_ref()
        .map(|tools| crate::tool_runtime::protocol::anthropic_tools_to_specs(tools.as_slice()))
        .unwrap_or_default();

    let payload_extraction = crate::tool_runtime::payload::extract_payloads(ouyi_output);
    let decision_text = payload_extraction.sanitized_text.as_str();

    let marked = parse_marked_tool_calls(decision_text, &tool_specs).map_err(AppError::BadRequest)?;
    if !marked.is_empty() {
        return Ok(BrokerDecision::ToolCalls(marked));
    }

    let rule_calls =
        parse_rule_tool_intent(decision_text, &tool_specs).map_err(AppError::BadRequest)?;
    if !rule_calls.is_empty() {
        return Ok(BrokerDecision::ToolCalls(rule_calls));
    }

    let Some(config) = tool_runtime_config else {
        return Ok(BrokerDecision::FinalAnswer { usage: None });
    };

    let compiled = match crate::tool_runtime::openai_compiler::compile_tool_decision_with_usage_and_payloads(
        http_client,
        config,
        req,
        decision_text,
        payload_extraction.payloads,
    )
    .await
    {
        Ok(compiled) => compiled,
        Err(e) => {
            tracing::warn!(error = %e, "工具协议编译失败，降级为主脑文本");
            return Ok(BrokerDecision::FinalAnswer { usage: None });
        }
    };

    match compiled.decision {
        crate::tool_runtime::openai_compiler::CompilerDecision::ToolCalls(calls) => {
            Ok(BrokerDecision::ToolCalls(
                calls
                    .into_iter()
                    .map(|call| BrokerToolCall {
                        id: call.id,
                        name: call.name,
                        input: call.input,
                    })
                    .collect(),
            ))
        }
        crate::tool_runtime::openai_compiler::CompilerDecision::FinalAnswer => {
            Ok(BrokerDecision::FinalAnswer {
                usage: Some(BrokerUsage {
                    prompt_tokens: compiled.usage.prompt_tokens,
                    completion_tokens: compiled.usage.completion_tokens,
                }),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_spec() -> ToolSpec {
        ToolSpec {
            name: "Bash".to_string(),
            description: "Run shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "description": {"type": "string"}
                },
                "required": ["command"]
            }),
        }
    }

    #[test]
    fn test_find_tool_by_name_is_case_sensitive() {
        let tools = vec![bash_spec()];
        let found = find_tool(&tools, "Bash").unwrap();
        assert_eq!(found.name, "Bash");
        assert!(find_tool(&tools, "bash").is_none());
    }

    #[test]
    fn test_parse_marked_ouyi_tool_call() {
        let tools = vec![bash_spec()];
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\",\"description\":\"print cwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let calls = parse_marked_tool_calls(output, &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "pwd");
        assert!(calls[0].id.starts_with("call_broker_"));
    }

    #[test]
    fn test_parse_marked_tool_call_skips_unknown_tool() {
        let tools = vec![bash_spec()];
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Nope\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let calls = parse_marked_tool_calls(output, &tools).unwrap();

        assert!(calls.is_empty());
    }

    #[test]
    fn test_rule_parse_bash_intent() {
        let tools = vec![bash_spec()];
        let output = "需要使用 Bash 执行命令：pwd";

        let calls = parse_rule_tool_intent(output, &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "pwd");
    }

    #[test]
    fn test_rule_parse_read_intent() {
        let tools = vec![ToolSpec {
            name: "Read".to_string(),
            description: "Read file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}},
                "required": ["file_path"]
            }),
        }];
        let output = "需要读取文件 probe.txt";

        let calls = parse_rule_tool_intent(output, &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].input["file_path"], "probe.txt");
    }

    #[test]
    fn test_rule_parse_bash_rejects_multiline_natural_language() {
        let tools = vec![bash_spec()];
        let calls =
            parse_rule_tool_intent("需要执行命令：pwd\n然后读取 probe.txt 分析结果", &tools)
                .unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn test_rule_parse_bash_accepts_single_line_command() {
        let tools = vec![bash_spec()];
        let calls = parse_rule_tool_intent("执行命令：pwd", &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "pwd");
    }

    #[test]
    fn test_rule_parse_bash_accepts_fenced_command() {
        let tools = vec![bash_spec()];
        let calls =
            parse_rule_tool_intent("执行命令：```powershell\nGet-Location\n```", &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "Get-Location");
    }

    #[test]
    fn test_detect_long_task_from_powershell_set_content() {
        let call = BrokerToolCall {
            id: "call_1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({
                "command": "powershell -NoProfile -Command \"Start-Sleep -Seconds 20; Set-Content -LiteralPath longtask.txt -Value DONE_MARKER\""
            }),
        };
        let plan = detect_long_task(&call).unwrap();
        assert_eq!(plan.file_path, "longtask.txt");
        assert_eq!(plan.done_marker, "DONE_MARKER");
        assert_eq!(plan.max_polls, 8);
    }

    #[test]
    fn test_build_poll_call_checks_done_marker() {
        let tools = vec![bash_spec()];
        let plan = LongTaskPlan {
            file_path: "longtask.txt".to_string(),
            done_marker: "DONE_MARKER".to_string(),
            interval_secs: 5,
            max_polls: 3,
        };
        let call = build_poll_call(&plan, &tools).unwrap();
        let command = call.input["command"].as_str().unwrap();
        assert!(command.contains("DONE_MARKER"));
        assert!(command.contains("PENDING"));
        assert!(command.contains("cat 'longtask.txt'"));
    }

    #[test]
    fn test_detect_long_task_from_bash_command() {
        let call = BrokerToolCall {
            id: "call_broker_1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({
                "command": "sleep 20 && echo DONE > longtask.txt",
                "description": "start long task"
            }),
        };

        let plan = detect_long_task(&call).unwrap();
        assert_eq!(plan.file_path, "longtask.txt");
        assert_eq!(plan.done_marker, "DONE");
    }

    #[test]
    fn test_build_poll_call_reads_expected_file() {
        let tools = vec![bash_spec()];
        let plan = LongTaskPlan {
            file_path: "longtask.txt".to_string(),
            done_marker: "DONE".to_string(),
            interval_secs: 5,
            max_polls: 8,
        };

        let call = build_poll_call(&plan, &tools).unwrap();
        assert_eq!(call.name, "Bash");
        assert!(
            call.input["command"]
                .as_str()
                .unwrap()
                .contains("longtask.txt")
        );
    }

    #[test]
    fn test_classify_long_task_result() {
        assert!(matches!(
            classify_long_task_result("PENDING", "DONE"),
            LongTaskResult::Pending
        ));
        assert!(matches!(
            classify_long_task_result("DONE", "DONE"),
            LongTaskResult::Done
        ));
        assert!(matches!(
            classify_long_task_result("some error output", "DONE"),
            LongTaskResult::Pending
        ));
    }
}
