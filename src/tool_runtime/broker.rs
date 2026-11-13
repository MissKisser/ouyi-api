//! 工具决策 broker：解析欧易主模型输出并裁决"调用工具还是给出最终回答"。
//! 解析按三级顺序进行：显式标记的工具调用 → 中文规则意图（"执行命令：/ 读取文件"）→
//! 可选的外部编译器（openai_compiler）；同时识别长时任务命令并生成轮询调用，
//! 全程对工具名、入参 schema 与 tool_choice 约束做校验。

use crate::config::ToolRuntimeConfig;
use crate::error::AppError;
use crate::model::anthropic::MessagesRequest;
use crate::tool_runtime::dialect::parse_known_tool_calls;
use crate::tool_runtime::protocol::{
    ToolSpec, anthropic_tool_choice_to_spec, normalize_tool_call_ids, validate_tool_choice,
};

fn broker_id() -> String {
    format!("call_broker_{}", uuid::Uuid::new_v4().simple())
}

/// broker 侧的 token 用量统计（来自编译器返回）。
#[derive(Debug, Clone)]
pub struct BrokerUsage {
    /// 编译器调用消耗的输入 token 数。
    pub prompt_tokens: u64,
    /// 编译器调用消耗的输出 token 数。
    pub completion_tokens: u64,
}

/// broker 对一轮主模型输出的裁决结果。
#[derive(Debug, Clone)]
pub enum BrokerDecision {
    /// 需要执行工具：携带解析并校验通过的工具调用列表。
    ToolCalls(Vec<BrokerToolCall>),
    /// 直接给出最终回答，不调用工具。
    FinalAnswer {
        /// 走编译器路径时的 token 用量；纯本地解析路径为 `None`。
        usage: Option<BrokerUsage>,
    },
}

/// broker 生成的单次工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerToolCall {
    /// 调用唯一标识，标记解析路径统一生成 `call_broker_` 前缀 ID。
    pub id: String,
    /// 工具名称（已在工具定义列表中校验存在）。
    pub name: String,
    /// 工具入参（已通过对应 schema 校验）。
    pub input: serde_json::Value,
}

/// 按名称查找工具定义。
///
/// # 参数
/// - `tools`：工具定义列表。
/// - `name`：目标工具名（区分大小写）。
///
/// # 返回
/// 命中的工具定义引用；不存在时为 `None`。
pub fn find_tool<'a>(tools: &'a [ToolSpec], name: &str) -> Option<&'a ToolSpec> {
    tools.iter().find(|tool| tool.name == name)
}

/// 解析显式标记包裹的工具调用。
///
/// 支持多种标记方言（见 dialect 模块），逐个调用校验工具名与入参 schema，
/// 并为每个调用生成 `call_broker_` 前缀的唯一 ID。
///
/// # 参数
/// - `ouyi_output`：欧易主模型的原始输出文本。
/// - `tools`：可用工具定义列表，用于校验。
///
/// # 返回
/// 校验通过的工具调用列表；文本无标记时为空列表。
///
/// # 错误
/// 标记内出现未知工具名、非法 JSON 或入参不符合 schema 时，返回描述性错误字符串。
pub fn parse_marked_tool_calls(
    ouyi_output: &str,
    tools: &[ToolSpec],
) -> Result<Vec<BrokerToolCall>, String> {
    let parsed = parse_known_tool_calls(ouyi_output)?;
    let mut calls = Vec::new();

    for parsed_call in parsed {
        let name = parsed_call.name;
        crate::tool_runtime::validation::validate_tool_call(tools, &name, &parsed_call.input)?;
        calls.push(BrokerToolCall {
            id: broker_id(),
            name,
            input: parsed_call.input,
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

/// 按中文规则从纯文本中识别工具意图并构造工具调用。
///
/// 当前支持两类规则（均要求对应工具在 `tools` 中存在）：
/// - "执行命令：" / "Bash 执行命令：" 后跟代码块或单行命令 → 调用 Bash；
/// - "读取文件 " / "读取文件：" / "Read 文件 " 后跟单行路径 → 调用 Read。
/// 多行自然语言不会命中 Bash 规则。
///
/// # 参数
/// - `ouyi_output`：欧易主模型的输出文本。
/// - `tools`：可用工具定义列表。
///
/// # 返回
/// 命中规则时返回恰好一个工具调用（带 `call_broker_` 前缀 ID）；未命中为空列表。
///
/// # 错误
/// 构造的入参未通过 schema 校验时返回错误描述。
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

/// 长时任务的轮询计划：描述如何检测一个后台任务是否完成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongTaskPlan {
    /// 任务完成标记所在的文件路径。
    pub file_path: String,
    /// 判定任务完成的标记文本（文件中出现即视为完成）。
    pub done_marker: String,
    /// 两次轮询之间的间隔秒数。
    pub interval_secs: u64,
    /// 最大轮询次数，超过后视为任务失败。
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

/// 识别工具调用中的长时任务命令并生成轮询计划。
///
/// 仅对 Bash 调用生效：命令需包含长时信号（`sleep` / `Start-Sleep` /
/// `run_in_background` / "后台"），并从 `Set-Content` 重定向或 `echo >`
/// 重定向中提取完成标记文件；未命中任一形态时返回 `None`。
///
/// # 参数
/// - `call`：待检查的工具调用。
///
/// # 返回
/// 命中时返回轮询计划（间隔 5 秒、最多轮询 8 次）；否则为 `None`。
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

/// 根据轮询计划构造一次 Bash 轮询调用。
///
/// 生成的命令先 sleep 一个间隔，再检查标记文件是否包含完成标记：
/// 命中则 `cat` 输出文件内容，否则回显 `PENDING`。
///
/// # 参数
/// - `plan`：长时任务轮询计划。
/// - `tools`：可用工具定义列表。
///
/// # 返回
/// 构造好的 Bash 工具调用（带 `call_broker_` 前缀 ID）。
///
/// # 错误
/// 工具列表中不存在 Bash 工具时返回错误描述。
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

/// 长时任务单次轮询的结果分类。
pub enum LongTaskResult {
    /// 任务尚未完成，需继续轮询（含输出为空或无法判定的情形）。
    Pending,
    /// 输出中出现完成标记，任务完成。
    Done,
    /// 轮询输出以 `FAILED:` 前缀报告了失败原因。
    Failed(String),
}

/// 按完成标记对轮询输出进行分类。
///
/// # 参数
/// - `content`：轮询命令的输出文本。
/// - `done_marker`：任务完成标记文本。
///
/// # 返回
/// 分类结果：含标记为 `Done`；以 `FAILED:` 开头为 `Failed`（携带原因）；
/// 其余（含 `PENDING` 或空输出）一律视为 `Pending`。
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

/// broker 决策主入口：综合三级解析路径，裁决调用工具还是直接回答。
///
/// 处理顺序：
/// 1. 提取输出中的工具调用负载并对剩余文本做显式意图检测；
/// 2. 解析显式标记的工具调用，命中即校验 tool_choice 后返回；
/// 3. 解析中文规则意图，命中即校验后返回；
/// 4. 未命中且编译器配置开启时调用外部编译器，对编译结果做工具调用校验、
///    tool_choice 校验与 ID 归一化后返回；
/// 5. 编译器未开启或判定为最终回答时，校验 tool_choice 约束后返回
///    `FinalAnswer`；若文本带有明确工具意图却未产生合法调用则报错。
///
/// # 参数
/// - `http_client`：编译器路径使用的 HTTP 客户端。
/// - `tool_runtime_config`：工具运行时配置；为 `None` 或未启用时跳过编译器。
/// - `req`：Anthropic Messages 请求（提供工具定义与 tool_choice）。
/// - `ouyi_output`：欧易主模型的原始输出文本。
///
/// # 返回
/// 裁决结果：工具调用列表或最终回答（编译器路径附带 token 用量）。
///
/// # 错误
/// 返回 `AppError::ToolProtocol`：标记解析失败、入参违反 schema、
/// tool_choice 约束不满足，或"有明确工具意图但未产生合法调用"。
/// 编译器路径的网络 / 上游错误按原样透传。
///
/// # 副作用
/// 走编译器路径时会发起一次外部 HTTP 调用并消耗其 token 配额。
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
    let tool_choice = anthropic_tool_choice_to_spec(req.tool_choice.as_ref());

    let payload_extraction = crate::tool_runtime::payload::extract_payloads(ouyi_output);
    let decision_text = payload_extraction.sanitized_text.as_str();
    let explicit_intent = crate::tool_runtime::dialect::has_explicit_tool_intent(decision_text);

    let mut marked =
        parse_marked_tool_calls(decision_text, &tool_specs).map_err(AppError::ToolProtocol)?;
    if !marked.is_empty() {
        let actual_names = marked
            .iter()
            .map(|call| call.name.clone())
            .collect::<Vec<_>>();
        validate_tool_choice(tool_choice.as_ref(), &actual_names)?;
        normalize_tool_call_ids(marked.iter_mut().map(|call| &mut call.id));
        return Ok(BrokerDecision::ToolCalls(marked));
    }

    let mut rule_calls =
        parse_rule_tool_intent(decision_text, &tool_specs).map_err(AppError::ToolProtocol)?;
    if !rule_calls.is_empty() {
        let actual_names = rule_calls
            .iter()
            .map(|call| call.name.clone())
            .collect::<Vec<_>>();
        validate_tool_choice(tool_choice.as_ref(), &actual_names)?;
        normalize_tool_call_ids(rule_calls.iter_mut().map(|call| &mut call.id));
        return Ok(BrokerDecision::ToolCalls(rule_calls));
    }

    let Some(config) = tool_runtime_config.filter(|config| config.enabled) else {
        validate_tool_choice(tool_choice.as_ref(), &[])?;
        if explicit_intent {
            return Err(AppError::ToolProtocol(
                "检测到明确工具意图，但没有生成合法工具调用".to_string(),
            ));
        }
        return Ok(BrokerDecision::FinalAnswer { usage: None });
    };

    let compiled =
        crate::tool_runtime::openai_compiler::compile_tool_decision_with_usage_and_payloads(
            http_client,
            config,
            req,
            decision_text,
            payload_extraction.payloads,
        )
        .await?;

    match compiled.decision {
        crate::tool_runtime::openai_compiler::CompilerDecision::ToolCalls(calls) => {
            for call in &calls {
                crate::tool_runtime::validation::validate_tool_call(
                    &tool_specs,
                    &call.name,
                    &call.input,
                )
                .map_err(AppError::ToolProtocol)?;
            }
            let mut calls = calls
                .into_iter()
                .map(|call| BrokerToolCall {
                    id: call.id,
                    name: call.name,
                    input: call.input,
                })
                .collect::<Vec<_>>();
            let actual_names = calls
                .iter()
                .map(|call| call.name.clone())
                .collect::<Vec<_>>();
            validate_tool_choice(tool_choice.as_ref(), &actual_names)?;
            normalize_tool_call_ids(calls.iter_mut().map(|call| &mut call.id));
            Ok(BrokerDecision::ToolCalls(calls))
        }
        crate::tool_runtime::openai_compiler::CompilerDecision::FinalAnswer => {
            validate_tool_choice(tool_choice.as_ref(), &[])?;
            if explicit_intent {
                Err(AppError::ToolProtocol(
                    "编译器未能转换明确的工具意图".to_string(),
                ))
            } else {
                Ok(BrokerDecision::FinalAnswer {
                    usage: Some(BrokerUsage {
                        prompt_tokens: compiled.usage.prompt_tokens,
                        completion_tokens: compiled.usage.completion_tokens,
                    }),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic_req_with_tools(
        tool_choice: crate::model::anthropic::ToolChoice,
    ) -> MessagesRequest {
        MessagesRequest {
            model: "claude-test".to_string(),
            messages: vec![crate::model::anthropic::AnthropicMessage {
                role: "user".to_string(),
                content: crate::model::anthropic::AnthropicContent::Text("完成任务".to_string()),
            }],
            system: None,
            max_tokens: 128,
            stream: Some(false),
            tools: Some(vec![
                crate::model::anthropic::AnthropicTool {
                    name: "Bash".to_string(),
                    description: "Run shell command".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"command": {"type": "string"}},
                        "required": ["command"]
                    }),
                },
                crate::model::anthropic::AnthropicTool {
                    name: "Read".to_string(),
                    description: "Read file".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"file_path": {"type": "string"}},
                        "required": ["file_path"]
                    }),
                },
            ]),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            tool_choice: Some(tool_choice),
        }
    }

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
    fn test_parse_marked_glm_tool_call() {
        let tools = vec![bash_spec()];
        let output = r#"<tool_call>Bash▁command▁params▁{"command":"pwd"}"#;

        let calls = parse_marked_tool_calls(output, &tools).unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "pwd");
    }

    #[test]
    fn test_parse_marked_tool_call_rejects_unknown_tool() {
        let tools = vec![bash_spec()];
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Nope\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let error = parse_marked_tool_calls(output, &tools).unwrap_err();

        assert!(error.contains("Nope"));
    }

    #[test]
    fn test_parse_marked_tool_call_rejects_schema_violation() {
        let tools = vec![bash_spec()];
        let output = r#"<tool_call>Bash▁command▁params▁{"description":"missing command"}"#;

        let error = parse_marked_tool_calls(output, &tools).unwrap_err();

        assert!(error.contains("command"));
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

    #[tokio::test]
    async fn test_anthropic_any_choice_rejects_final_answer_without_compiler() {
        let req = anthropic_req_with_tools(crate::model::anthropic::ToolChoice::Any);

        let err = decide_with_broker(
            reqwest::Client::new(),
            None,
            &req,
            "这是最终回答，没有工具调用。",
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, AppError::ToolProtocol(message) if message.contains("expected=required") && message.contains("actual=<none>"))
        );
    }

    #[tokio::test]
    async fn test_anthropic_specific_choice_rejects_other_valid_marked_tool() {
        let req = anthropic_req_with_tools(crate::model::anthropic::ToolChoice::Tool {
            name: "Read".to_string(),
        });
        let output = concat!(
            "<|tool▁calls▁begin|>",
            "[{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}]",
            "<|tool▁calls▁end|>"
        );

        let err = decide_with_broker(reqwest::Client::new(), None, &req, output)
            .await
            .unwrap_err();

        assert!(
            matches!(err, AppError::ToolProtocol(message) if message.contains("expected=Read") && message.contains("actual=Bash"))
        );
    }
}
