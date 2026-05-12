use crate::model::anthropic::AnthropicTool;

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
}
