use crate::adapter::tool_call::ToolCallParser;
use crate::model::anthropic::AnthropicTool;

fn broker_id() -> String {
    format!("call_broker_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerDecision {
    ToolCalls(Vec<BrokerToolCall>),
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

pub fn find_tool<'a>(tools: &'a [AnthropicTool], name: &str) -> Option<&'a AnthropicTool> {
    tools.iter().find(|tool| tool.name == name)
}

pub fn parse_marked_tool_calls(
    ouyi_output: &str,
    tools: &[AnthropicTool],
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
            return Err(format!("未知工具: {}", name));
        }
        let arguments = func
            .arguments
            .unwrap_or_else(|| "{}".to_string());
        let input: serde_json::Value = serde_json::from_str(&arguments)
            .map_err(|e| format!("工具参数不是合法 JSON: {}", e))?;
        calls.push(BrokerToolCall {
            id: broker_id(),
            name,
            input,
        });
    }

    Ok(calls)
}

pub fn parse_rule_tool_intent(
    ouyi_output: &str,
    tools: &[AnthropicTool],
) -> Result<Vec<BrokerToolCall>, String> {
    let text = ouyi_output.trim();

    if find_tool(tools, "Bash").is_some() {
        for marker in ["执行命令：", "执行命令:", "Bash 执行命令：", "Bash 执行命令:"] {
            if let Some((_, command)) = text.rsplit_once(marker) {
                let command = command.trim().trim_matches('`').trim();
                if !command.is_empty() {
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

    Some(LongTaskPlan {
        file_path,
        done_marker,
        interval_secs: 5,
        max_polls: 8,
    })
}

pub fn build_poll_call(
    plan: &LongTaskPlan,
    tools: &[AnthropicTool],
) -> Result<BrokerToolCall, String> {
    if find_tool(tools, "Bash").is_none() {
        return Err("长时任务轮询需要 Bash 工具".to_string());
    }
    let command = format!(
        "sleep {}; if test -f '{}'; then cat '{}'; else echo PENDING; fi",
        plan.interval_secs,
        plan.file_path.replace('\'', "'\\''"),
        plan.file_path.replace('\'', "'\\''")
    );
    Ok(BrokerToolCall {
        id: broker_id(),
        name: "Bash".to_string(),
        input: serde_json::json!({
            "command": command,
            "description": format!("检查长时任务是否输出 {}", plan.done_marker)
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
    } else if content.contains("PENDING") || content.trim().is_empty() {
        LongTaskResult::Pending
    } else {
        LongTaskResult::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_tool() -> AnthropicTool {
        AnthropicTool {
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
        let tools = vec![bash_tool()];
        let found = find_tool(&tools, "Bash").unwrap();
        assert_eq!(found.name, "Bash");
        assert!(find_tool(&tools, "bash").is_none());
    }

    #[test]
    fn test_parse_marked_ouyi_tool_call() {
        let tools = vec![bash_tool()];
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
    fn test_parse_marked_tool_call_rejects_unknown_tool() {
        let tools = vec![bash_tool()];
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Nope\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let err = parse_marked_tool_calls(output, &tools).unwrap_err();
        assert!(err.contains("未知工具"));
    }

    #[test]
    fn test_rule_parse_bash_intent() {
        let tools = vec![bash_tool()];
        let output = "需要使用 Bash 执行命令：pwd";

        let calls = parse_rule_tool_intent(output, &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "pwd");
    }

    #[test]
    fn test_rule_parse_read_intent() {
        let tools = vec![AnthropicTool {
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
        let tools = vec![bash_tool()];
        let plan = LongTaskPlan {
            file_path: "longtask.txt".to_string(),
            done_marker: "DONE".to_string(),
            interval_secs: 5,
            max_polls: 8,
        };

        let call = build_poll_call(&plan, &tools).unwrap();
        assert_eq!(call.name, "Bash");
        assert!(call.input["command"].as_str().unwrap().contains("longtask.txt"));
    }

    #[test]
    fn test_classify_long_task_result() {
        assert!(matches!(classify_long_task_result("PENDING", "DONE"), LongTaskResult::Pending));
        assert!(matches!(classify_long_task_result("DONE", "DONE"), LongTaskResult::Done));
        assert!(matches!(classify_long_task_result("some error output", "DONE"), LongTaskResult::Pending));
    }
}
