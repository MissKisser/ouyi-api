//! 模型名映射器：维护"客户端短名 ↔ 欧易完整模型名"的双向映射表。
//! 支持从欧易 8.0 模板响应或模型目录加载映射，加载失败时回退到内置的
//! 静态模型列表；并提供按前缀模糊匹配解析模型名、按供应商分组列举等
//! 查询能力，供路由与模型列表接口使用。

use dashmap::DashMap;
use serde::Serialize;

use super::catalog::ModelCatalog;
use super::ouyi::{TmplCapabilities, TmplModel, TmplResponse};
use super::ouyi::{TmplData, TmplModelAttr, TmplProvider};

/// 官网模型分类项（按 providerName 分组）。
#[derive(Debug, Clone, Serialize)]
pub struct ModelCategoryItem {
    /// 模型展示名。
    pub label: String,
    /// 模型完整标识（欧易侧的 value）。
    pub value: String,
    /// 供应商标识 key。
    pub provider_key: String,
    /// 模型能力位。
    pub capabilities: TmplCapabilities,
}

/// 按供应商分组后的模型列表分组。
#[derive(Debug, Clone, Serialize)]
pub struct ModelCategory {
    /// 供应商展示名称。
    pub provider_name: String,
    /// 供应商标识 key。
    pub provider_key: String,
    /// 该分组下的模型数量。
    pub count: usize,
    /// 该分组下的模型条目列表。
    pub models: Vec<ModelCategoryItem>,
}

/// 映射表中单个模型的完整记录。
#[derive(Debug, Clone)]
pub struct ModelEntry {
    /// 欧易侧完整模型名，如 `claude_chat::claude-sonnet-4-5-vip`。
    pub full_value: String,
    /// 对外暴露的短名，如 `claude-sonnet-4-5-vip`。
    pub short_name: String,
    /// 供应商标识 key。
    pub provider_key: String,
    /// 模型能力位（当前解析流程未消费，保留供后续使用）。
    #[allow(dead_code)]
    pub capabilities: TmplCapabilities,
}

/// 模型名映射器，线程安全（内部为并发哈希表）。
/// 维护短名到完整名、完整名到短名的双向映射，以及完整模型元数据表。
#[derive(Clone)]
pub struct ModelMapper {
    short_to_full: DashMap<String, ModelEntry>,
    full_to_short: DashMap<String, String>,
    all_models: DashMap<String, TmplModel>,
}

static FALLBACK_MODELS: &[(&str, &str, &str)] = &[
    (
        "claude_chat::claude-sonnet-4-5-20250929-vip",
        "claude-sonnet-4-5",
        "claude_chat",
    ),
    ("openai_chat::gpt-4o", "gpt-4o", "openai_chat"),
    ("openai_chat::gpt-4o-mini", "gpt-4o-mini", "openai_chat"),
    ("deepseek_chat::deepseek-r1", "deepseek-r1", "deepseek_chat"),
    (
        "gemini_chat::gemini-2.0-flash",
        "gemini-2.0-flash",
        "gemini_chat",
    ),
];

impl ModelMapper {
    /// 创建一个空的映射器（不含任何模型，需再调用 load_* 装载）。
    pub fn new() -> Self {
        Self {
            short_to_full: DashMap::new(),
            full_to_short: DashMap::new(),
            all_models: DashMap::new(),
        }
    }

    /// 从欧易 8.0 模板响应装载映射表。
    ///
    /// 为每个模型生成短名及其别名（去掉 `-vip` / `-thinking` 后缀的形式），
    /// 解决短名碰撞后整体替换现有映射，并输出加载统计日志。
    /// 响应缺 `data` 或 `models` 为空时改用静态 fallback 列表。
    ///
    /// # 参数
    /// - `resp`：欧易 8.0 模板接口响应。
    ///
    /// # 副作用
    /// 清空并重建内部映射表；输出 warn/info 级日志。
    pub fn load_from_tmpl(&self, resp: &TmplResponse) {
        let data = match &resp.data {
            Some(d) => d,
            None => {
                tracing::warn!("模型列表响应 data 为空，使用静态fallback");
                self.load_fallback();
                return;
            }
        };
        let mut new_short_to_full: Vec<(String, ModelEntry)> = Vec::new();
        let mut new_full_to_short: Vec<(String, String)> = Vec::new();
        let mut new_all_models: Vec<(String, TmplModel)> = Vec::new();

        if data.models.is_empty() {
            tracing::warn!("模型列表响应 models 为空，使用静态fallback");
            self.load_fallback();
            return;
        }

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

    /// 从模型目录装载映射表。
    ///
    /// 将目录条目转换为 Tmpl 模型结构后复用 `load_from_tmpl` 的装载流程；
    /// 目录为空时改用静态 fallback 列表。
    ///
    /// # 参数
    /// - `catalog`：由 catalog 模块构建的模型目录。
    ///
    /// # 副作用
    /// 清空并重建内部映射表；目录为空时输出 warn 级日志。
    pub fn load_from_catalog(&self, catalog: &ModelCatalog) {
        if catalog.models.is_empty() {
            tracing::warn!("模型目录为空，使用静态fallback");
            self.load_fallback();
            return;
        }

        let models = catalog
            .models
            .iter()
            .map(|item| TmplModel {
                label: item.label.clone(),
                value: item.id.clone(),
                attr: TmplModelAttr {
                    capabilities: item.capabilities.clone(),
                    provider_key: item.provider_key.clone(),
                    provider_name: item.provider_name.clone(),
                    integral: String::new(),
                    model_type: String::new(),
                },
            })
            .collect();

        let resp = TmplResponse {
            code: 0,
            msg: String::new(),
            data: Some(TmplData {
                models,
                providers: Vec::<TmplProvider>::new(),
                def_model: String::new(),
                default_chat: String::new(),
                cm: false,
                gen_line: 0,
                gen_title: false,
                m_file_count: 0,
                m_file_size: 0,
                mcp: Vec::new(),
            }),
        };

        self.load_from_tmpl(&resp);
    }

    /// 装载内置的静态 fallback 模型列表。
    ///
    /// 在远端模型列表不可用（响应缺数据、目录为空）时兜底使用，
    /// 固定能力位为：流式与系统角色开启，其余关闭。
    ///
    /// # 副作用
    /// 清空并重建内部映射表；输出 warn 级日志。
    pub fn load_fallback(&self) {
        tracing::warn!("使用静态fallback模型列表");

        self.short_to_full.clear();
        self.full_to_short.clear();
        self.all_models.clear();

        for &(full, short, provider) in FALLBACK_MODELS {
            let capabilities = TmplCapabilities {
                image_input: false,
                any_file: false,
                tools: false,
                stream: true,
                system_role: true,
                thinking: false,
                auto_detect_thinking: false,
            };
            let entry = ModelEntry {
                full_value: full.to_string(),
                short_name: short.to_string(),
                provider_key: provider.to_string(),
                capabilities: capabilities.clone(),
            };
            self.short_to_full.insert(short.to_string(), entry);
            self.full_to_short
                .insert(full.to_string(), short.to_string());
            self.all_models.insert(
                full.to_string(),
                TmplModel {
                    label: short.to_string(),
                    value: full.to_string(),
                    attr: TmplModelAttr {
                        capabilities,
                        provider_key: provider.to_string(),
                        provider_name: provider_key_to_name(provider),
                        integral: String::new(),
                        model_type: String::new(),
                    },
                },
            );
        }
    }

    /// 将客户端输入的模型名解析为欧易侧完整模型名。
    ///
    /// 解析顺序：精确短名匹配 → 完整名原样返回 → 短名前缀匹配。
    /// 前缀匹配命中多个时优先带 `-vip` 的条目，其次按短名倒序取最大者；
    /// 全部未命中时原样返回输入并记录 warn 日志。
    ///
    /// # 参数
    /// - `input`：客户端传入的模型名（短名、完整名或前缀）。
    ///
    /// # 返回
    /// 欧易侧完整模型名；无法解析时返回原始输入。
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
                b_vip
                    .cmp(&a_vip)
                    .then_with(|| b.short_name.cmp(&a.short_name))
            });
            return matches[0].full_value.clone();
        }

        tracing::warn!("模型名映射失败，使用原始输入: {}", input);
        input.to_string()
    }

    /// 返回全部已注册的模型短名，按字典序排序。
    pub fn get_all_short_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .full_to_short
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        names.sort();
        names
    }

    /// 查询完整模型名对应的供应商展示名称。
    ///
    /// # 参数
    /// - `full_value`：欧易侧完整模型名。
    ///
    /// # 返回
    /// 供应商展示名称；模型不在映射表中时为 `None`。
    pub fn get_provider_name(&self, full_value: &str) -> Option<String> {
        self.all_models
            .get(full_value)
            .map(|m| m.attr.provider_name.clone())
    }

    /// 返回当前映射表中的模型总数（按完整模型名去重）。
    pub fn model_count(&self) -> usize {
        self.all_models.len()
    }

    /// 判断指定模型的输出是否应按思考（thinking）内容处理。
    ///
    /// 对输入做解析后检查完整模型名或原始输入中是否包含 `-thinking` 标记。
    ///
    /// # 参数
    /// - `model_input`：客户端传入的模型名。
    ///
    /// # 返回
    /// `true` 表示该模型名带思考标记。
    pub fn should_thinking(&self, model_input: &str) -> bool {
        let resolved = self.to_ouyi(model_input);
        resolved.contains("-thinking") || model_input.contains("-thinking")
    }

    /// 按官网 providerName 分组返回所有模型。
    ///
    /// 以（provider_name, provider_key）为分组键聚合全部模型，
    /// 结果按分组键字典序排列，便于前端按供应商分栏展示。
    ///
    /// # 返回
    /// 供应商分组列表，每组含模型条目与计数。
    pub fn get_categorized_models(&self) -> Vec<ModelCategory> {
        use std::collections::BTreeMap;

        // 按 (provider_name, provider_key) 分组
        let mut groups: BTreeMap<String, Vec<ModelCategoryItem>> = BTreeMap::new();
        let mut provider_keys: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for entry in self.all_models.iter() {
            let tmpl = entry.value();
            let label = tmpl.label.clone();
            let value = tmpl.value.clone();
            let provider_key = tmpl.attr.provider_key.clone();
            let provider_name = tmpl.attr.provider_name.clone();
            let capabilities = tmpl.attr.capabilities.clone();

            let group_key = format!("{}|{}", provider_name, provider_key);
            provider_keys
                .entry(provider_name.clone())
                .or_insert_with(|| provider_key.clone());

            groups
                .entry(group_key)
                .or_default()
                .push(ModelCategoryItem {
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
        assert_eq!(mapper.to_ouyi("openai_chat::gpt-4o"), "openai_chat::gpt-4o");
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

    #[test]
    fn test_load_from_catalog_maps_models() {
        let mapper = ModelMapper::new();
        let catalog = ModelCatalog {
            platform: crate::platform::OuyiPlatform::Ouyi7,
            models: vec![super::super::catalog::ModelCatalogItem {
                id: "claude-sonnet-4-6".to_string(),
                label: "Claude Sonnet 4.6".to_string(),
                provider_key: "claude".to_string(),
                provider_name: "Claude".to_string(),
                capabilities: TmplCapabilities {
                    stream: true,
                    system_role: true,
                    ..TmplCapabilities::default()
                },
                metadata: None,
            }],
        };

        mapper.load_from_catalog(&catalog);

        assert_eq!(mapper.to_ouyi("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!(mapper.model_count(), 1);
        assert_eq!(
            mapper.get_provider_name("claude-sonnet-4-6").as_deref(),
            Some("Claude")
        );
    }
}
