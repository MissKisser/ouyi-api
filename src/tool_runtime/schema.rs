//! 面向编译器的工具 schema 改写。
//! 把 Write/Edit/MultiEdit 等工具入参 schema 中的长文本字段（content、
//! old_string、new_string）替换为对应的 `*_ref` 引用字段，使编译器输出的
//! 长正文通过 payload 引用传递；其余工具 schema 原样保留。

use crate::tool_runtime::protocol::ToolSpec;

/// 改写单个工具 schema 以适配编译器输出约定。
///
/// 改写规则：
/// - `Write`：`content` 字段替换为 `content_ref`；
/// - `Edit`：`old_string` / `new_string` 替换为对应的 `*_ref`；
/// - `MultiEdit`：`edits[].items` 内的两个文本字段替换为 `*_ref`；
/// - 其它工具原样克隆返回。
///
/// `required` 列表中的同名项同步替换；schema 结构不符合预期时保持原样。
///
/// # 参数
/// - `spec`：原始工具规格。
///
/// # 返回
/// 改写后的工具规格副本。
pub fn compiler_tool_spec(spec: &ToolSpec) -> ToolSpec {
    match spec.name.as_str() {
        "Write" => rewrite_object_fields(spec, &[("content", "content_ref")]),
        "Edit" => rewrite_object_fields(
            spec,
            &[
                ("old_string", "old_string_ref"),
                ("new_string", "new_string_ref"),
            ],
        ),
        "MultiEdit" => rewrite_multiedit(spec),
        _ => spec.clone(),
    }
}

/// 批量改写工具 schema 列表（逐项调用 [`compiler_tool_spec`]）。
///
/// # 参数
/// - `specs`：原始工具规格列表。
///
/// # 返回
/// 改写后的工具规格列表，顺序与输入一致。
pub fn compiler_tool_specs(specs: &[ToolSpec]) -> Vec<ToolSpec> {
    specs.iter().map(compiler_tool_spec).collect()
}

fn rewrite_object_fields(spec: &ToolSpec, fields: &[(&str, &str)]) -> ToolSpec {
    let mut rewritten = spec.clone();
    let Some(obj) = rewritten.input_schema.as_object_mut() else {
        return rewritten;
    };

    if let Some(properties) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
        for (field, ref_field) in fields {
            properties.remove(*field);
            properties.insert(
                (*ref_field).to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "引用欧亿主脑输出中的 ouyi_payload id"
                }),
            );
        }
    }

    if let Some(required) = obj.get_mut("required").and_then(|v| v.as_array_mut()) {
        for value in required.iter_mut() {
            if let Some(name) = value.as_str() {
                if let Some((_, ref_field)) = fields.iter().find(|(field, _)| *field == name) {
                    *value = serde_json::Value::String((*ref_field).to_string());
                }
            }
        }
    }

    rewritten
}

fn rewrite_multiedit(spec: &ToolSpec) -> ToolSpec {
    let mut rewritten = spec.clone();
    let items = rewritten
        .input_schema
        .get_mut("properties")
        .and_then(|v| v.as_object_mut())
        .and_then(|props| props.get_mut("edits"))
        .and_then(|edits| edits.get_mut("items"));

    let Some(items) = items else {
        return rewritten;
    };

    if let Some(properties) = items.get_mut("properties").and_then(|v| v.as_object_mut()) {
        properties.remove("old_string");
        properties.remove("new_string");
        properties.insert(
            "old_string_ref".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "引用欧亿主脑输出中的 ouyi_payload id"
            }),
        );
        properties.insert(
            "new_string_ref".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "引用欧亿主脑输出中的 ouyi_payload id"
            }),
        );
    }

    if let Some(required) = items.get_mut("required").and_then(|v| v.as_array_mut()) {
        for value in required.iter_mut() {
            if value.as_str() == Some("old_string") {
                *value = serde_json::Value::String("old_string_ref".to_string());
            } else if value.as_str() == Some("new_string") {
                *value = serde_json::Value::String("new_string_ref".to_string());
            }
        }
    }

    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_runtime::protocol::ToolSpec;

    fn write_spec() -> ToolSpec {
        ToolSpec {
            name: "Write".to_string(),
            description: "Write file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    #[test]
    fn rewrites_write_content_to_content_ref() {
        let rewritten = compiler_tool_spec(&write_spec());

        assert!(
            rewritten.input_schema["properties"]
                .get("content")
                .is_none()
        );
        assert_eq!(
            rewritten.input_schema["properties"]["content_ref"]["description"],
            "引用欧亿主脑输出中的 ouyi_payload id"
        );
        assert_eq!(
            rewritten.input_schema["required"],
            serde_json::json!(["file_path", "content_ref"])
        );
    }

    #[test]
    fn keeps_read_schema_unchanged() {
        let read = ToolSpec {
            name: "Read".to_string(),
            description: "Read file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}},
                "required": ["file_path"]
            }),
        };

        assert_eq!(compiler_tool_spec(&read), read);
    }

    #[test]
    fn rewrites_multiedit_item_fields() {
        let spec = ToolSpec {
            name: "MultiEdit".to_string(),
            description: "Edit multiple places".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": {"type": "string"},
                                "new_string": {"type": "string"}
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["file_path", "edits"]
            }),
        };

        let rewritten = compiler_tool_spec(&spec);
        let item_props = &rewritten.input_schema["properties"]["edits"]["items"]["properties"];

        assert!(item_props.get("old_string").is_none());
        assert!(item_props.get("new_string").is_none());
        assert!(item_props.get("old_string_ref").is_some());
        assert!(item_props.get("new_string_ref").is_some());
    }
}
