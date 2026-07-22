//! 探针：绕过 minimax 编译器，直接验证 broker 解析层对"主脑应该吐的"工具调用标记的解析能力。
//!
//! 跑法：cargo test --bin ouyi-api tool_runtime::broker_probe -- --nocapture
//!
//! 包含 13 个场景，覆盖理想格式、含 <think> 前缀、并发多工具、长任务、未知工具、
//! 非法 JSON、纯文本、尾部解释、中文 command、嵌套参数、中文标记符等。

use crate::model::openai::ToolDef;
use crate::tool_runtime::broker::parse_marked_tool_calls;
use crate::tool_runtime::protocol::openai_tools_to_specs;
use serde_json::json;

#[test]
fn probe_broker_tool_call_parsing() {
    let tools = vec![ToolDef {
        tool_type: "function".to_string(),
        function: crate::model::openai::ToolFunction {
            name: "Bash".to_string(),
            description: Some("Run a shell command".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "run_in_background": {"type": "boolean"}
                },
                "required": ["command"]
            })),
        },
    }];
    let specs = openai_tools_to_specs(&tools);
    println!("\nToolSpec: {} (count={})", specs[0].name, specs.len());

    run(
        "S1 ideal array form",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "ls -la"}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S2 single object form",
        r#"<|tool▁calls▁begin|>
{"name": "Bash", "arguments": {"command": "pwd"}}
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S3 with <think> prefix",
        r#"<think>用户让我列目录，我用 Bash。</think>
<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "ls"}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S4 parallel tool calls",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "ls"}}, {"name": "Bash", "arguments": {"command": "pwd"}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S5 long task run_in_background=true",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "npm run dev", "run_in_background": true}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S6 long task with sleep",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "sleep 30 && echo done"}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S7 unknown tool name (should be skipped)",
        r#"<|tool▁calls▁begin|>
[{"name": "MysteryTool", "arguments": {"x": 1}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S8 malformed JSON (should skip this entry)",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": "{not valid json"}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S9 plain text (no marks, expect 0)",
        "好的，我直接回答你：今天天气不错。",
        &specs,
    );

    run(
        "S10 trailing text after marks (rule violation)",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "ls"}}]
<|tool▁calls▁end|>
我已经执行了命令，请看结果。"#,
        &specs,
    );

    run(
        "S11 chinese command value",
        &r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "echo 你好世界"}}]
<|tool▁alls▁end|>"#
            .replace("▁alls▁", "▁calls▁"),
        &specs,
    );

    run(
        "S12 nested args object",
        r#"<|tool▁calls▁begin|>
[{"name": "Bash", "arguments": {"command": "git", "extra": {"timeout": 30, "env": ["DEBUG=1"]}}}]
<|tool▁calls▁end|>"#,
        &specs,
    );

    run(
        "S13 chinese variant markers",
        r#"【工具调用】
{"name": "Bash", "arguments": {"command": "ls"}}
【调用结束】"#,
        &specs,
    );
}

fn run(label: &str, text: &str, specs: &[crate::tool_runtime::protocol::ToolSpec]) {
    println!("\n-- {}", label);
    match parse_marked_tool_calls(text, specs) {
        Ok(calls) => {
            println!("   ok: {} calls", calls.len());
            for c in &calls {
                println!("     id={} name={} input={}", c.id, c.name, c.input);
                if let Some(p) = crate::tool_runtime::broker::detect_long_task(c) {
                    println!(
                        "     long_task: file={} marker={} max_polls={}",
                        p.file_path, p.done_marker, p.max_polls
                    );
                }
            }
        }
        Err(e) => println!("   err: {}", e),
    }
}
