//! 推理入口 API Key 鉴权模块：key 规范化、掩码与常数时间比较。

/// API Key 的最小长度；更短的 key 在管理端保存时被整体拒绝。
pub const MIN_KEY_LEN: usize = 16;

/// 规范化 API Key 列表：逐项去空白；空白项与掩码占位项忽略（表示保留已有 key）；
/// 短于 [`MIN_KEY_LEN`] 的项使整次更新失败；结果去重。
/// 返回 `Err(String)`：描述第一个过短的项。
pub fn normalize_keys(raw: &[String]) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = Vec::new();
    for item in raw {
        let key = item.trim();
        if key.is_empty() || is_masked(key) {
            continue;
        }
        if key.len() < MIN_KEY_LEN {
            return Err(format!(
                "API key 长度不足 {MIN_KEY_LEN} 个字符: {}",
                mask_key(key)
            ));
        }
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    Ok(keys)
}

/// 判断值是否为掩码占位（与账号 token 掩码约定一致：包含 `…` 且以非空白字符起止）。
fn is_masked(value: &str) -> bool {
    value.contains('…') || value.contains("****")
}

/// 生成脱敏展示形式：保留前 3 与后 4 个字符，中间以 `…` 连接；
/// 长度不足 9 个字符时整体以 `****` 代替。
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// 常数时间字符串比较，避免 key 比对被计时侧信道利用。
/// 长度不同直接返回 `false`（key 长度不视为秘密）。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keys_trims_dedupes_and_rejects_short() {
        let ok = normalize_keys(&[
            "  sk-alpha-0123456789abcdef  ".to_string(),
            "sk-alpha-0123456789abcdef".to_string(),
        ])
        .unwrap();
        assert_eq!(ok, vec!["sk-alpha-0123456789abcdef".to_string()]);

        let err = normalize_keys(&["sk-short".to_string()]).unwrap_err();
        assert!(err.contains("长度不足"));
    }

    #[test]
    fn normalize_keys_ignores_blank_and_masked_entries() {
        let ok = normalize_keys(&[
            String::new(),
            "   ".to_string(),
            "sk-…6789".to_string(),
            "sk-beta-0123456789abcdef".to_string(),
        ])
        .unwrap();
        assert_eq!(ok, vec!["sk-beta-0123456789abcdef".to_string()]);
    }

    #[test]
    fn mask_key_keeps_head_and_tail() {
        assert_eq!(mask_key("sk-alpha-0123456789abcdef"), "sk-…cdef");
        assert_eq!(mask_key("short"), "****");
    }

    #[test]
    fn constant_time_eq_matches_only_identical_values() {
        assert!(constant_time_eq("sk-a-0123456789abcdef", "sk-a-0123456789abcdef"));
        assert!(!constant_time_eq("sk-a-0123456789abcdef", "sk-b-0123456789abcdef"));
        assert!(!constant_time_eq("short", "sk-a-0123456789abcdef"));
    }
}
