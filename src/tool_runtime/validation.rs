//! 工具调用入参的 schema 校验。
//! 按 JSON Schema 的常用子集（type / enum / required / properties /
//! additionalProperties / items）递归校验工具调用入参，拦截未知工具名、
//! 缺失必填字段、类型不匹配、枚举越界与多余字段。

use crate::tool_runtime::protocol::ToolSpec;

/// 校验一次工具调用的名称与入参。
///
/// # 参数
/// - `tools`：可用工具定义列表。
/// - `name`：工具调用名称。
/// - `input`：工具调用入参 JSON。
///
/// # 返回
/// 校验通过返回 `Ok(())`。
///
/// # 错误
/// 工具名不存在、工具定义重名，或入参不符合 schema（含缺失必填字段、
/// 类型不匹配、枚举越界、数组项违规、多余字段）时，返回带路径的
/// 描述性错误字符串。
pub fn validate_tool_call(
    tools: &[ToolSpec],
    name: &str,
    input: &serde_json::Value,
) -> Result<(), String> {
    let matches: Vec<&ToolSpec> = tools.iter().filter(|tool| tool.name == name).collect();
    let spec = match matches.as_slice() {
        [] => return Err(format!("未知工具: {name}")),
        [spec] => *spec,
        _ => return Err(format!("工具定义重名: {name}")),
    };
    validate_value(&spec.input_schema, input, name)
}

fn validate_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    if let Some(allowed) = schema.get("enum").and_then(serde_json::Value::as_array)
        && !allowed.contains(value)
    {
        return Err(format!("{path} 不在允许的枚举值中"));
    }

    let Some(expected_type) = schema.get("type").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let type_matches = match expected_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    };
    if !type_matches {
        return Err(format!("{path} 类型必须是 {expected_type}"));
    }

    match expected_type {
        "object" => validate_object(schema, value, path),
        "array" => validate_array(schema, value, path),
        _ => Ok(()),
    }
}

fn validate_object(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path} 必须是 object"))?;
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for field in required.iter().filter_map(serde_json::Value::as_str) {
            if !object.contains_key(field) {
                return Err(format!("{path}.{field} 是必填字段"));
            }
        }
    }

    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    for (field, field_value) in object {
        if let Some(field_schema) = properties.and_then(|properties| properties.get(field)) {
            validate_value(field_schema, field_value, &format!("{path}.{field}"))?;
        } else if schema
            .get("additionalProperties")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            return Err(format!("{path}.{field} 不是允许的字段"));
        }
    }
    Ok(())
}

fn validate_array(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    let Some(item_schema) = schema.get("items") else {
        return Ok(());
    };
    for (index, item) in value.as_array().into_iter().flatten().enumerate() {
        validate_value(item_schema, item, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_runtime::protocol::ToolSpec;

    fn grep_spec() -> ToolSpec {
        ToolSpec {
            name: "Grep".to_string(),
            description: "Search files".to_string(),
            input_schema: serde_json::json!({
                "type":"object",
                "properties":{
                    "pattern":{"type":"string"},
                    "path":{"type":"string"},
                    "mode":{"type":"string","enum":["files","content"]},
                    "include":{"type":"array","items":{"type":"string"}}
                },
                "required":["pattern"],
                "additionalProperties":false
            }),
        }
    }

    #[test]
    fn accepts_generic_tool_arguments() {
        validate_tool_call(
            &[grep_spec()],
            "Grep",
            &serde_json::json!({"pattern":"TODO","mode":"files","include":["*.rs"]}),
        )
        .unwrap();
    }

    #[test]
    fn rejects_missing_required_property() {
        let error = validate_tool_call(&[grep_spec()], "Grep", &serde_json::json!({})).unwrap_err();
        assert!(error.contains("pattern"));
    }

    #[test]
    fn rejects_wrong_type_enum_array_item_and_extra_property() {
        for input in [
            serde_json::json!({"pattern":1}),
            serde_json::json!({"pattern":"x","mode":"other"}),
            serde_json::json!({"pattern":"x","include":[1]}),
            serde_json::json!({"pattern":"x","unknown":true}),
        ] {
            assert!(validate_tool_call(&[grep_spec()], "Grep", &input).is_err());
        }
    }

    #[test]
    fn rejects_unknown_and_duplicate_tool_definitions() {
        assert!(validate_tool_call(&[grep_spec()], "Read", &serde_json::json!({})).is_err());
        assert!(
            validate_tool_call(
                &[grep_spec(), grep_spec()],
                "Grep",
                &serde_json::json!({"pattern":"x"})
            )
            .is_err()
        );
    }
}
