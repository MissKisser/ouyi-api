use crate::model::ouyi::{TmplCapabilities, TmplResponse};
use crate::model::ouyi_v7::{MemberInfoResult, Ouyi7EnumItem};
use crate::platform::OuyiPlatform;
use serde::{Deserialize, Serialize};

/// 模型目录
#[derive(Debug, Clone)]
pub struct ModelCatalog {
    pub platform: OuyiPlatform,
    pub models: Vec<ModelCatalogItem>,
}

/// 模型目录项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogItem {
    pub id: String,
    pub label: String,
    pub provider_key: String,
    pub provider_name: String,
    pub capabilities: TmplCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ModelCatalog {
    /// 从 8.0 TmplResponse 转换
    pub fn from_tmpl(resp: &TmplResponse) -> Self {
        let models = resp
            .data
            .as_ref()
            .map(|data| {
                data.models
                    .iter()
                    .map(|m| ModelCatalogItem {
                        id: m.value.clone(),
                        label: m.label.clone(),
                        provider_key: m.attr.provider_key.clone(),
                        provider_name: m.attr.provider_name.clone(),
                        capabilities: m.attr.capabilities.clone(),
                        metadata: None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        ModelCatalog {
            platform: OuyiPlatform::Ouyi8,
            models,
        }
    }

    /// 从 7.0 数据转换
    ///
    /// 实地 JWT 探测确认的后端模型路由机制：
    /// - `privileges[].customAIModels`（字符串名，如 "deepseek-chat"）→ 用 `roleId=1` + `params.model="字符串名"` 路由
    /// - `privileges[].aiModels`（数字 value 数组，对应 sysEnum）→ 用 `roleId=value` 路由
    ///
    /// 因此 catalog 只收录这两类用户实际订阅的模型，不再无脑加全部 sysEnum
    /// （旧实现会把 GPT-3.5 系列、Claude 1/2、ERNIE 等已淘汰/无权的 66 个模型全列出来）。
    /// 路由信息存进 `metadata`，由 client 的路由表读取。
    pub fn from_ouyi_v7(member_info: &MemberInfoResult, enum_items: &[Ouyi7EnumItem]) -> Self {
        let mut models = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. customAIModels —— 走 roleId=1 + params.model 路由
        if let Some(group) = &member_info.group_info {
            for priv_item in &group.privileges {
                for model_name in &priv_item.custom_ai_models {
                    if !seen.insert(model_name.clone()) {
                        continue;
                    }
                    let (provider_key, provider_name) = infer_provider_from_model_name(model_name);
                    models.push(ModelCatalogItem {
                        id: model_name.clone(),
                        label: model_name.clone(),
                        provider_key,
                        provider_name,
                        capabilities: infer_ouyi_v7_capabilities(model_name),
                        metadata: Some(serde_json::json!({
                            "routing": "custom",
                            "model": model_name,
                        })),
                    });
                }

                // 2. aiModels（数字 value）—— 走 roleId=value 路由
                // 在 enum_items 里查到对应的 name/describe 作为展示名
                for role_id in &priv_item.ai_models {
                    let Some(enum_item) = enum_items.iter().find(|it| {
                        it.value.as_i64() == Some(*role_id)
                    }) else {
                        // enum 里找不到对应 value（可能是后端新加的模型），用数字兜底
                        let synthetic_id = format!("role_{}", role_id);
                        if !seen.insert(synthetic_id.clone()) {
                            continue;
                        }
                        models.push(ModelCatalogItem {
                            id: synthetic_id,
                            label: format!("Role {}", role_id),
                            provider_key: "ouyi7".to_string(),
                            provider_name: "Ouyi 7.0".to_string(),
                            capabilities: default_ouyi_v7_capabilities(),
                            metadata: Some(serde_json::json!({
                                "routing": "standard",
                                "role_id": role_id,
                            })),
                        });
                        continue;
                    };

                    // 用 describe（人类可读、符合 OpenAI 标准命名，如 "gpt-4o"/"gpt-3.5-turbo"/"claude-2"）
                    // 作 id —— 与 customAIModels 风格一致、与 8.0 列表一致、与用户认知一致。
                    // name（如 "ChatGPT4o"）只是后端内部枚举 key，不应对外暴露。
                    // 实地探测确认 describe 与 customAIModels 零重叠，去重安全。
                    let id = if enum_item.describe.is_empty() {
                        enum_item.name.clone()
                    } else {
                        enum_item.describe.clone()
                    };
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    let label = if enum_item.describe.is_empty() {
                        enum_item.name.clone()
                    } else {
                        enum_item.describe.clone()
                    };
                    let (provider_key, provider_name) = infer_provider_from_model_name(&label);
                    models.push(ModelCatalogItem {
                        id,
                        label,
                        provider_key,
                        provider_name,
                        capabilities: infer_ouyi_v7_capabilities(&enum_item.describe),
                        metadata: Some(serde_json::json!({
                            "routing": "standard",
                            "role_id": role_id,
                            "name": enum_item.name,
                            "value": enum_item.value,
                        })),
                    });
                }
            }
        }

        ModelCatalog {
            platform: OuyiPlatform::Ouyi7,
            models,
        }
    }
}

/// 7.0 默认能力位（无法从后端拿到真实 capabilities 时的兜底）
pub fn default_ouyi_v7_capabilities() -> TmplCapabilities {
    TmplCapabilities {
        stream: false,
        tools: false,
        thinking: false,
        system_role: true,
        image_input: false,
        any_file: false,
        auto_detect_thinking: false,
    }
}

/// 根据模型名推断 7.0 模型的能力位。
///
/// 7.0 后端的 memberInfo/sysEnum 都不暴露 capabilities 元数据，
/// 但模型名本身包含明显的能力暗示（实测订阅的 60 个 customAIModels + 29 个 aiModels）：
/// - thinking：deepseek-r1/reasoner、qwq-32b、grok-3-reasoning、glm-4.1v-thinking、o1/o3 系列等
/// - image_input：gpt-4o、gpt-4-vision、glm-4v、qwen-vl、grok-2-vision 等
/// - stream/tools 仍按 false（7.0 上游不支持原生 SSE/tools，由反代侧模拟/外挂）
pub fn infer_ouyi_v7_capabilities(model_name: &str) -> TmplCapabilities {
    let name_lower = model_name.to_lowercase();
    let thinking = is_thinking_model(&name_lower);
    let image_input = is_vision_model(&name_lower);
    TmplCapabilities {
        stream: false,
        tools: false,
        thinking,
        system_role: true,
        image_input,
        any_file: false,
        auto_detect_thinking: thinking,
    }
}

fn is_thinking_model(name_lower: &str) -> bool {
    const THINKING_KEYWORDS: &[&str] = &[
        "r1", "reasoner", "reasoning", "thinking", "qwq", "o1-", "o3-", "o1preview",
        "o1mini", "deepseek-r", "glm-z1",
    ];
    THINKING_KEYWORDS.iter().any(|kw| {
        // 词边界匹配：避免 "r1" 误匹配 "gpt-3.5-turbo-1106" 之类
        name_lower == *kw
            || name_lower.contains(&format!("-{}", kw))
            || name_lower.contains(&format!("{}-", kw))
            || name_lower.contains(&format!("/{}", kw))
    })
}

fn is_vision_model(name_lower: &str) -> bool {
    const VISION_KEYWORDS: &[&str] = &[
        "gpt-4o", "gpt-4-vision", "vision", "-vl", "-4v", "image", "claude-3",
        "claude-3.5", "claude-3-5", "gemini", "qwen-vl", "glm-4v", "grok-2-vision",
        "grok-3-image", "glm-4.1v",
    ];
    VISION_KEYWORDS.iter().any(|kw| name_lower.contains(kw))
}

/// 根据模型名推断供应商
fn infer_provider_from_model_name(name: &str) -> (String, String) {
    let name_lower = name.to_lowercase();

    if name_lower.starts_with("gpt-")
        || name_lower.starts_with("o1-")
        || name_lower.starts_with("o3-")
    {
        ("openai".to_string(), "OpenAI".to_string())
    } else if name_lower.starts_with("claude-") {
        ("claude".to_string(), "Claude".to_string())
    } else if name_lower.starts_with("deepseek-") {
        ("deepseek".to_string(), "DeepSeek".to_string())
    } else if name_lower.contains("gemini") {
        ("google".to_string(), "Google".to_string())
    } else if name_lower.starts_with("llama-") || name_lower.contains("meta") {
        ("meta".to_string(), "Meta".to_string())
    } else if name_lower.starts_with("qwen-") || name_lower.contains("alibaba") {
        ("alibaba".to_string(), "Alibaba".to_string())
    } else {
        ("ouyi7".to_string(), "Ouyi 7.0".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_ouyi_v7_capabilities() {
        let caps = default_ouyi_v7_capabilities();
        assert!(!caps.stream);
        assert!(!caps.tools);
        assert!(!caps.thinking);
        assert!(caps.system_role);
        assert!(!caps.image_input);
        assert!(!caps.any_file);
        assert!(!caps.auto_detect_thinking);
    }

    #[test]
    fn test_infer_provider_openai() {
        let (key, name) = infer_provider_from_model_name("gpt-4");
        assert_eq!(key, "openai");
        assert_eq!(name, "OpenAI");

        let (key, _name) = infer_provider_from_model_name("o1-preview");
        assert_eq!(key, "openai");
    }

    #[test]
    fn test_infer_provider_claude() {
        let (key, name) = infer_provider_from_model_name("claude-3-sonnet");
        assert_eq!(key, "claude");
        assert_eq!(name, "Claude");
    }

    #[test]
    fn test_infer_provider_deepseek() {
        let (key, name) = infer_provider_from_model_name("deepseek-chat");
        assert_eq!(key, "deepseek");
        assert_eq!(name, "DeepSeek");
    }

    #[test]
    fn test_infer_provider_unknown() {
        let (key, name) = infer_provider_from_model_name("unknown-model");
        assert_eq!(key, "ouyi7");
        assert_eq!(name, "Ouyi 7.0");
    }

    #[test]
    fn test_catalog_item_serialization() {
        let item = ModelCatalogItem {
            id: "gpt-4".to_string(),
            label: "GPT-4".to_string(),
            provider_key: "openai".to_string(),
            provider_name: "OpenAI".to_string(),
            capabilities: default_ouyi_v7_capabilities(),
            metadata: None,
        };

        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"id\":\"gpt-4\""));
        assert!(json.contains("\"label\":\"GPT-4\""));
    }

    /// 回归：旧 from_ouyi_v7 会把 sysEnum 全部模型塞进 catalog，
    /// 新逻辑只收录 customAIModels + aiModels 订阅的。
    #[test]
    fn from_ouyi_v7_only_includes_subscribed_models_not_all_enums() {
        use crate::model::ouyi_v7::{GroupInfo, MemberInfoResult, Privilege};

        // 用户订阅：1 个 customAIModel + 1 个标准模型（gpt-4o = value 35）
        let member = MemberInfoResult {
            group_info: Some(GroupInfo {
                privileges: vec![Privilege {
                    custom_ai_models: vec!["deepseek-chat".to_string()],
                    ai_models: vec![35],
                }],
            }),
        };
        // sysEnum 里有 3 个模型（含用户没订阅的 gpt-3.5 = value 10）
        // describe 用后端真实风格的小写名（如 "gpt-4o"），name 是后端内部枚举 key（如 "ChatGPT4o"）
        let enums = vec![
            Ouyi7EnumItem {
                describe: "gpt-4o".to_string(),
                name: "ChatGPT4o".to_string(),
                value: serde_json::json!(35),
            },
            Ouyi7EnumItem {
                describe: "gpt-3.5-turbo".to_string(),
                name: "ChatGPTTurbo".to_string(),
                value: serde_json::json!(10),
            },
            Ouyi7EnumItem {
                describe: "claude-3-opus-20240229".to_string(),
                name: "Claude3_Opus".to_string(),
                value: serde_json::json!(102),
            },
        ];

        let catalog = ModelCatalog::from_ouyi_v7(&member, &enums);

        // 应该只有 2 个模型（deepseek-chat + gpt-4o），gpt-3.5/claude3 不在内
        assert_eq!(catalog.models.len(), 2, "只收录订阅的，不应有未订阅的旧模型");

        let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"deepseek-chat"), "customAIModel 应被收录");
        assert!(
            ids.contains(&"gpt-4o"),
            "标准模型 id 应该是 describe（人类可读），实际: {ids:?}"
        );
        assert!(
            !ids.contains(&"ChatGPT4o"),
            "标准模型 id 不应是后端内部 name，实际: {ids:?}"
        );
        assert!(
            !ids.iter().any(|s| s.contains("turbo") || s.contains("claude")),
            "未订阅的旧模型不应出现"
        );

        // customAIModel 的 metadata 应标记为 "custom"
        let ds = catalog.models.iter().find(|m| m.id == "deepseek-chat").unwrap();
        assert_eq!(ds.metadata.as_ref().unwrap()["routing"], "custom");
        assert_eq!(ds.metadata.as_ref().unwrap()["model"], "deepseek-chat");

        // 标准模型 metadata 应标记为 "standard" + role_id + name（内部 key 保留）
        let gpt4o = catalog.models.iter().find(|m| m.id == "gpt-4o").unwrap();
        assert_eq!(gpt4o.metadata.as_ref().unwrap()["routing"], "standard");
        assert_eq!(gpt4o.metadata.as_ref().unwrap()["role_id"], 35);
        assert_eq!(gpt4o.metadata.as_ref().unwrap()["name"], "ChatGPT4o");
        // label 用 describe（人类可读）
        assert_eq!(gpt4o.label, "gpt-4o");
    }

    /// capabilities 推断：思考型模型 + 视觉模型应被正确识别
    #[test]
    fn infer_ouyi_v7_capabilities_detects_thinking_and_vision() {
        // thinking
        assert!(infer_ouyi_v7_capabilities("deepseek-r1").thinking);
        assert!(infer_ouyi_v7_capabilities("deepseek-reasoner").thinking);
        assert!(infer_ouyi_v7_capabilities("qwq-32b").thinking);
        assert!(infer_ouyi_v7_capabilities("grok-3-reasoning").thinking);
        // 非思考型
        assert!(!infer_ouyi_v7_capabilities("deepseek-chat").thinking);
        assert!(!infer_ouyi_v7_capabilities("gpt-4o").thinking);
        // 词边界：gpt-3.5-turbo-1106 不应被 -r1- 之类的误匹配
        assert!(!infer_ouyi_v7_capabilities("gpt-3.5-turbo-1106").thinking);

        // vision
        assert!(infer_ouyi_v7_capabilities("gpt-4o-2024-11-20").image_input);
        assert!(infer_ouyi_v7_capabilities("glm-4v-flash").image_input);
        assert!(infer_ouyi_v7_capabilities("qwen-vl-plus").image_input);
        assert!(infer_ouyi_v7_capabilities("claude-3-5-sonnet-20241022-vip").image_input);
        // 非视觉
        assert!(!infer_ouyi_v7_capabilities("deepseek-chat").image_input);
    }
}
