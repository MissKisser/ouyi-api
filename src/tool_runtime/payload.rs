#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPayload {
    pub id: String,
    pub body: String,
    pub byte_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadExtraction {
    pub sanitized_text: String,
    pub payloads: Vec<ToolPayload>,
}

const OPEN_TAG: &str = "<ouyi_payload";
const CLOSE_TAG: &str = "</ouyi_payload>";

pub fn extract_payloads(input: &str) -> PayloadExtraction {
    let mut sanitized = String::with_capacity(input.len());
    let mut payloads = Vec::new();
    let mut rest = input;

    while let Some(open_pos) = rest.find(OPEN_TAG) {
        sanitized.push_str(&rest[..open_pos]);
        let after_open_start = &rest[open_pos..];
        let Some(open_end) = after_open_start.find('>') else {
            sanitized.push_str(after_open_start);
            return PayloadExtraction { sanitized_text: sanitized, payloads };
        };
        let open_tag = &after_open_start[..=open_end];
        let body_start = open_pos + open_end + 1;
        let after_body_start = &rest[body_start..];
        let Some(close_pos) = after_body_start.find(CLOSE_TAG) else {
            sanitized.push_str(after_open_start);
            return PayloadExtraction { sanitized_text: sanitized, payloads };
        };

        let raw_body = &after_body_start[..close_pos];
        let body = trim_wrapper_newline(raw_body).to_string();
        let base_id = attr_value(open_tag, "id").unwrap_or_else(|| format!("p{}", payloads.len()));
        let id = unique_payload_id(&payloads, &base_id);
        let byte_len = body.len();
        payloads.push(ToolPayload {
            id: id.clone(),
            body,
            byte_len,
        });
        sanitized.push_str(&format!(
            "<ouyi_payload_ref id=\"{}\" bytes=\"{}\" />",
            id, byte_len
        ));

        let close_end = body_start + close_pos + CLOSE_TAG.len();
        rest = &rest[close_end..];
    }

    sanitized.push_str(rest);
    PayloadExtraction {
        sanitized_text: sanitized,
        payloads,
    }
}

fn trim_wrapper_newline(raw: &str) -> &str {
    let without_start = raw.strip_prefix("\r\n").or_else(|| raw.strip_prefix('\n')).unwrap_or(raw);
    without_start
        .strip_suffix("\r\n")
        .or_else(|| without_start.strip_suffix('\n'))
        .unwrap_or(without_start)
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

fn unique_payload_id(existing: &[ToolPayload], base: &str) -> String {
    if !existing.iter().any(|p| p.id == base) {
        return base.to_string();
    }
    let mut idx = 2;
    loop {
        let candidate = format!("{base}_{idx}");
        if !existing.iter().any(|p| p.id == candidate) {
            return candidate;
        }
        idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_payload_and_replaces_body_with_ref() {
        let input = "准备写入：\n<ouyi_payload id=\"p1\">\n# 标题\n\"quote\"\n</ouyi_payload>\n完成。";

        let extracted = extract_payloads(input);

        assert_eq!(extracted.payloads.len(), 1);
        assert_eq!(extracted.payloads[0].id, "p1");
        assert_eq!(extracted.payloads[0].body, "# 标题\n\"quote\"");
        assert!(extracted.sanitized_text.contains("<ouyi_payload_ref id=\"p1\" bytes=\"16\" />"));
        assert!(!extracted.sanitized_text.contains("# 标题"));
    }

    #[test]
    fn keeps_unclosed_payload_as_plain_text() {
        let input = "text <ouyi_payload id=\"p1\">abc";

        let extracted = extract_payloads(input);

        assert!(extracted.payloads.is_empty());
        assert_eq!(extracted.sanitized_text, input);
    }

    #[test]
    fn generates_unique_id_when_missing_or_duplicate() {
        let input = concat!(
            "<ouyi_payload>alpha</ouyi_payload>\n",
            "<ouyi_payload id=\"p0\">beta</ouyi_payload>\n",
            "<ouyi_payload id=\"p0\">gamma</ouyi_payload>"
        );

        let extracted = extract_payloads(input);
        let ids: Vec<&str> = extracted.payloads.iter().map(|p| p.id.as_str()).collect();

        assert_eq!(ids, vec!["p0", "p0_2", "p0_3"]);
    }
}
