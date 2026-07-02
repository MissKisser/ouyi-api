use crate::tool_runtime::payload::ToolPayload;

const LONG_TEXT_LIMIT: usize = 1024;

const REF_FIELDS: &[(&str, &str)] = &[
    ("content_ref", "content"),
    ("old_string_ref", "old_string"),
    ("new_string_ref", "new_string"),
    ("command_ref", "command"),
];

const LONG_TEXT_FIELDS: &[&str] = &["content", "old_string", "new_string", "command"];

pub fn materialize_payload_refs(
    value: &mut serde_json::Value,
    payloads: &[ToolPayload],
) -> Result<(), String> {
    match value {
        serde_json::Value::Object(obj) => {
            for key in LONG_TEXT_FIELDS {
                if let Some(text) = obj.get(*key).and_then(|v| v.as_str())
                    && text.len() > LONG_TEXT_LIMIT
                {
                    return Err("长文本字段需要使用 payload 引用".to_string());
                }
            }

            for (ref_key, target_key) in REF_FIELDS {
                if let Some(ref_value) = obj.remove(*ref_key) {
                    if obj.contains_key(*target_key) {
                        return Err(format!("{target_key} 与 {ref_key} 不能同时存在"));
                    }
                    let payload_id = ref_value
                        .as_str()
                        .ok_or_else(|| format!("{ref_key} 必须是字符串"))?;
                    let payload = payloads
                        .iter()
                        .find(|payload| payload.id == payload_id)
                        .ok_or_else(|| format!("payload 引用不存在: {payload_id}"))?;
                    obj.insert(
                        (*target_key).to_string(),
                        serde_json::Value::String(payload.body.clone()),
                    );
                }
            }

            for child in obj.values_mut() {
                materialize_payload_refs(child, payloads)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                materialize_payload_refs(item, payloads)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_runtime::payload::ToolPayload;

    fn payloads() -> Vec<ToolPayload> {
        vec![
            ToolPayload {
                id: "p1".to_string(),
                body: "# 报告\n\"quote\"".to_string(),
                byte_len: "# 报告\n\"quote\"".len(),
            },
            ToolPayload {
                id: "old".to_string(),
                body: "old text".to_string(),
                byte_len: "old text".len(),
            },
            ToolPayload {
                id: "new".to_string(),
                body: "new text".to_string(),
                byte_len: "new text".len(),
            },
        ]
    }

    #[test]
    fn materializes_write_content_ref() {
        let mut input = serde_json::json!({
            "file_path": "docs/report.md",
            "content_ref": "p1"
        });

        materialize_payload_refs(&mut input, &payloads()).unwrap();

        assert_eq!(input["content"], "# 报告\n\"quote\"");
        assert!(input.get("content_ref").is_none());
    }

    #[test]
    fn materializes_long_payload_ref_without_length_rejection() {
        let body = "x".repeat(1500);
        let payloads = vec![ToolPayload {
            id: "long".to_string(),
            body: body.clone(),
            byte_len: body.len(),
        }];
        let mut input = serde_json::json!({
            "file_path": "docs/report.md",
            "content_ref": "long"
        });

        materialize_payload_refs(&mut input, &payloads).unwrap();

        assert_eq!(input["content"], body);
        assert!(input.get("content_ref").is_none());
    }

    #[test]
    fn materializes_nested_multiedit_refs() {
        let mut input = serde_json::json!({
            "file_path": "src/lib.rs",
            "edits": [
                {
                    "old_string_ref": "old",
                    "new_string_ref": "new"
                }
            ]
        });

        materialize_payload_refs(&mut input, &payloads()).unwrap();

        assert_eq!(input["edits"][0]["old_string"], "old text");
        assert_eq!(input["edits"][0]["new_string"], "new text");
    }

    #[test]
    fn rejects_unknown_ref() {
        let mut input = serde_json::json!({"content_ref": "missing"});

        let err = materialize_payload_refs(&mut input, &payloads()).unwrap_err();

        assert!(err.contains("payload 引用不存在"));
    }

    #[test]
    fn rejects_long_direct_content_from_tool_model() {
        let mut input = serde_json::json!({
            "content": "x".repeat(1500)
        });

        let err = materialize_payload_refs(&mut input, &payloads()).unwrap_err();

        assert!(err.contains("长文本字段需要使用 payload 引用"));
    }
}
