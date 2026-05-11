use dashmap::DashMap;
use serde::Serialize;

use super::ouyi::{TmplCapabilities, TmplModel, TmplResponse};

/// 官网模型分类项（按 providerName 分组）
#[derive(Debug, Clone, Serialize)]
pub struct ModelCategoryItem {
    pub label: String,
    pub value: String,
    pub provider_key: String,
    pub capabilities: TmplCapabilities,
}

/// 按 providerName 分组的模型列表
#[derive(Debug, Clone, Serialize)]
pub struct ModelCategory {
    pub provider_name: String,
    pub provider_key: String,
    pub count: usize,
    pub models: Vec<ModelCategoryItem>,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub full_value: String,
    pub short_name: String,
    pub provider_key: String,
    #[allow(dead_code)]
    pub capabilities: TmplCapabilities,
}

#[derive(Clone)]
pub struct ModelMapper {
    short_to_full: DashMap<String, ModelEntry>,
    full_to_short: DashMap<String, String>,
    all_models: DashMap<String, TmplModel>,
}

static FALLBACK_MODELS: &[(&str, &str, &str)] = &[
    ("claude_chat::claude-sonnet-4-5-20250929-vip", "claude-sonnet-4-5", "claude_chat"),
    ("openai_chat::gpt-4o", "gpt-4o", "openai_chat"),
    ("openai_chat::gpt-4o-mini", "gpt-4o-mini", "openai_chat"),
    ("deepseek_chat::deepseek-r1", "deepseek-r1", "deepseek_chat"),
    ("gemini_chat::gemini-2.0-flash", "gemini-2.0-flash", "gemini_chat"),
];

impl ModelMapper {
    pub fn new() -> Self {
        Self {
            short_to_full: DashMap::new(),
            full_to_short: DashMap::new(),
            all_models: DashMap::new(),
        }
    }

    pub fn load_from_tmpl(&self, resp: &TmplResponse) {
        let data = match &resp.data {
            Some(d) => d,
            None => return,
        };
        let mut new_short_to_full: Vec<(String, ModelEntry)> = Vec::new();
        let mut new_full_to_short: Vec<(String, String)> = Vec::new();
        let mut new_all_models: Vec<(String, TmplModel)> = Vec::new();

        for model in &data.models {
            let full_value = model.value.clone();
            let provider_key = if model.attr.provider_key.is_empty() {
                extract_provider_from_value(&full_value)
            } else {
                model.attr.provider_key.clone()
            };
            let short_name = generate_short_name(&full_value, &provider_key);

            let entry = ModelEntry {
                full_value: full_value.clone(),
                short_name: short_name.clone(),
                provider_key: provider_key.clone(),
                capabilities: model.attr.capabilities.clone(),
            };

            new_short_to_full.push((short_name.clone(), entry));
            new_full_to_short.push((full_value.clone(), short_name.clone()));

            let capabilities = model.attr.capabilities.clone();
            let mut model = model.clone();
            if model.attr.provider_key.is_empty() {
                model.attr.provider_key = provider_key.clone();
            }
            if model.attr.provider_name.is_empty() {
                model.attr.provider_name = provider_key_to_name(&provider_key);
            }

            for alias in generate_aliases(&short_name) {
                let alias_entry = ModelEntry {
                    full_value: full_value.clone(),
                    short_name: alias.clone(),
                    provider_key: provider_key.clone(),
                    capabilities: capabilities.clone(),
                };
                new_short_to_full.push((alias, alias_entry));
            }

            new_all_models.push((full_value.clone(), model));
        }

        resolve_collisions(&mut new_short_to_full);

        self.short_to_full.clear();
        self.full_to_short.clear();
        self.all_models.clear();

        for (k, v) in new_short_to_full {
            self.short_to_full.insert(k, v);
        }
        for (k, v) in new_full_to_short {
            self.full_to_short.insert(k, v);
        }
        for (k, v) in new_all_models {
            self.all_models.insert(k, v);
        }

        tracing::info!(
            "模型映射已加载: {} 个模型, {} 个短名映射",
            self.all_models.len(),
            self.short_to_full.len()
        );
    }

    pub fn load_fallback(&self) {
        tracing::warn!("使用静态fallback模型列表");

        for &(full, short, provider) in FALLBACK_MODELS {
            let entry = ModelEntry {
                full_value: full.to_string(),
                short_name: short.to_string(),
                provider_key: provider.to_string(),
                capabilities: TmplCapabilities {
                    image_input: false,
                    any_file: false,
                    tools: false,
                    stream: true,
                    system_role: true,
                    thinking: false,
                    auto_detect_thinking: false,
                },
            };
            self.short_to_full.insert(short.to_string(), entry);
            self.full_to_short.insert(full.to_string(), short.to_string());
        }
    }

    pub fn to_ouyi(&self, input: &str) -> String {
        if let Some(entry) = self.short_to_full.get(input) {
            return entry.full_value.clone();
        }

        if self.full_to_short.contains_key(input) {
            return input.to_string();
        }

        let lower_input = input.to_lowercase();
        let mut matches: Vec<ModelEntry> = Vec::new();
        for entry in self.short_to_full.iter() {
            if entry.key().to_lowercase().starts_with(&lower_input) {
                matches.push(entry.value().clone());
            }
        }

        if matches.len() == 1 {
            return matches[0].full_value.clone();
        }

        if matches.len() > 1 {
            matches.sort_by(|a, b| {
                let a_vip = a.short_name.contains("-vip");
                let b_vip = b.short_name.contains("-vip");
                b_vip.cmp(&a_vip).then_with(|| b.short_name.cmp(&a.short_name))
            });
            return matches[0].full_value.clone();
        }

        tracing::warn!("模型名映射失败，使用原始输入: {}", input);
        input.to_string()
    }

    pub fn get_all_short_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .full_to_short
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        names.sort();
        names
    }

    pub fn get_provider_name(&self, full_value: &str) -> Option<String> {
        self.all_models
            .get(full_value)
            .map(|m| m.attr.provider_name.clone())
    }

    pub fn model_count(&self) -> usize {
        self.all_models.len()
    }

    pub fn should_thinking(&self, model_input: &str) -> bool {
        let resolved = self.to_ouyi(model_input);
        resolved.contains("-thinking") || model_input.contains("-thinking")
    }

    /// 按官网 providerName 分组返回所有模型
    pub fn get_categorized_models(&self) -> Vec<ModelCategory> {
        use std::collections::BTreeMap;

        // 按 (provider_name, provider_key) 分组
        let mut groups: BTreeMap<String, Vec<ModelCategoryItem>> = BTreeMap::new();
        let mut provider_keys: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        for entry in self.all_models.iter() {
            let tmpl = entry.value();
            let label = tmpl.label.clone();
            let value = tmpl.value.clone();
            let provider_key = tmpl.attr.provider_key.clone();
            let provider_name = tmpl.attr.provider_name.clone();
            let capabilities = tmpl.attr.capabilities.clone();

            let group_key = format!("{}|{}", provider_name, provider_key);
            provider_keys.entry(provider_name.clone()).or_insert_with(|| provider_key.clone());

            groups.entry(group_key).or_default().push(ModelCategoryItem {
                label,
                value,
                provider_key,
                capabilities,
            });
        }

        groups
            .into_iter()
            .map(|(key, models)| {
                let parts: Vec<&str> = key.splitn(2, '|').collect();
                let provider_name = parts[0].to_string();
                let provider_key = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
                let count = models.len();
                ModelCategory {
                    provider_name,
                    provider_key,
                    count,
                    models,
                }
            })
            .collect()
    }
}

fn generate_short_name(value: &str, provider_key: &str) -> String {
    let prefix = format!("{}::", provider_key);
    if let Some(stripped) = value.strip_prefix(&prefix) {
        stripped.to_string()
    } else if let Some(pos) = value.find("::") {
        value[pos + 2..].to_string()
    } else {
        value.to_string()
    }
}

fn generate_aliases(short_name: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for suffix in &["-vip", "-thinking"] {
        if let Some(stripped) = short_name.strip_suffix(suffix) {
            aliases.push(stripped.to_string());
        }
    }
    aliases
}

fn extract_provider_from_value(value: &str) -> String {
    if let Some(pos) = value.find("::") {
        value[..pos].to_string()
    } else {
        "unknown".to_string()
    }
}

fn provider_key_to_name(key: &str) -> String {
    match key {
        "claude_chat" | "anthropic_chat" => "Claude".to_string(),
        "openai_chat" => "OpenAI".to_string(),
        "deepseek_chat" => "DeepSeek".to_string(),
        "gemini_chat" | "google_chat" => "Gemini".to_string(),
        "baichuan_chat" => "Baichuan".to_string(),
        "zhipu_chat" | "chatglm" => "Zhipu".to_string(),
        "qwen_chat" | "aliyun_chat" => "Qwen".to_string(),
        "yi_chat" => "Yi".to_string(),
        "mistral_chat" => "Mistral".to_string(),
        "llama_chat" | "meta_chat" => "Llama".to_string(),
        "moonshot_chat" => "Moonshot".to_string(),
        "minimax_chat" => "MiniMax".to_string(),
        "stepfun_chat" => "StepFun".to_string(),
        "cohere_chat" => "Cohere".to_string(),
        _ => {
            let mut chars = key.chars();
            match chars.next() {
                None => key.to_string(),
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    format!("{upper}{}", chars.as_str())
                }
            }
        }
    }
}

fn resolve_collisions(entries: &mut [(String, ModelEntry)]) {
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for (name, entry) in entries.iter_mut() {
        if let Some(existing_provider) = seen.get(name.as_str()) {
            if existing_provider != &entry.provider_key {
                let new_name = format!("{}_{}", entry.provider_key, name);
                tracing::debug!(
                    "短名碰撞: {} ({} vs {}), 回退为 {}",
                    name,
                    existing_provider,
                    entry.provider_key,
                    new_name
                );
                entry.short_name = new_name.clone();
                *name = new_name;
            }
        } else {
            seen.insert(name.clone(), entry.provider_key.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_short_name() {
        assert_eq!(
            generate_short_name("claude_chat::claude-sonnet-4-5", "claude_chat"),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            generate_short_name("openai_chat::gpt-4o", "openai_chat"),
            "gpt-4o"
        );
    }

    #[test]
    fn test_generate_aliases() {
        let aliases = generate_aliases("claude-sonnet-4-5-vip");
        assert!(aliases.contains(&"claude-sonnet-4-5".to_string()));

        let aliases = generate_aliases("deepseek-r1-thinking");
        assert!(aliases.contains(&"deepseek-r1".to_string()));
    }

    #[test]
    fn test_mapper_basic() {
        let mapper = ModelMapper::new();
        mapper.load_fallback();

        assert_eq!(mapper.to_ouyi("gpt-4o"), "openai_chat::gpt-4o");
        assert_eq!(
            mapper.to_ouyi("openai_chat::gpt-4o"),
            "openai_chat::gpt-4o"
        );
    }

    #[test]
    fn test_mapper_prefix_match() {
        let mapper = ModelMapper::new();
        mapper.load_fallback();

        let result = mapper.to_ouyi("gpt-4");
        assert!(result.starts_with("openai_chat::"));
    }

    #[test]
    fn test_mapper_no_match() {
        let mapper = ModelMapper::new();
        mapper.load_fallback();

        assert_eq!(mapper.to_ouyi("nonexistent-model"), "nonexistent-model");
    }
}
