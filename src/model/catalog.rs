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
    pub fn from_ouyi_v7(member_info: &MemberInfoResult, enum_items: &[Ouyi7EnumItem]) -> Self {
        let mut models = Vec::new();

        // 添加 customAIModels
        if let Some(group) = &member_info.group_info {
            for priv_item in &group.privileges {
                for model_name in &priv_item.custom_ai_models {
                    let (provider_key, provider_name) = infer_provider_from_model_name(model_name);
                    models.push(ModelCatalogItem {
                        id: model_name.clone(),
                        label: model_name.clone(),
                        provider_key,
                        provider_name,
                        capabilities: default_ouyi_v7_capabilities(),
                        metadata: None,
                    });
                }
            }
        }

        // 添加系统枚举模型
        for item in enum_items {
            // 去重：如果已存在相同 ID，跳过
            if models.iter().any(|m| m.id == item.describe) {
                continue;
            }

            let (provider_key, provider_name) = infer_provider_from_model_name(&item.describe);
            models.push(ModelCatalogItem {
                id: item.describe.clone(),
                label: item.describe.clone(),
                provider_key,
                provider_name,
                capabilities: default_ouyi_v7_capabilities(),
                metadata: Some(serde_json::json!({
                    "name": item.name,
                    "value": item.value
                })),
            });
        }

        ModelCatalog {
            platform: OuyiPlatform::Ouyi7,
            models,
        }
    }
}

/// 7.0 默认能力
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
}
