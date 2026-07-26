//! 模型目录 DTO 与 Provider Profile 校验。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::model::profile::DEFAULT_MODEL_CAPABILITY_DEFAULTS;

/// 模型目录变更错误。
#[derive(Debug)]
pub enum ModelCatalogError {
    Validation(String),
    NotFound(String),
    Conflict(String),
    Config(crate::app::preferences::MuseConfigStoreError),
}

impl std::fmt::Display for ModelCatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(message) | Self::NotFound(message) | Self::Conflict(message) => {
                formatter.write_str(message)
            }
            Self::Config(error) => write!(formatter, "模型配置发布失败：{error}"),
        }
    }
}

impl std::error::Error for ModelCatalogError {}

/// 前端配置页使用的模型目录快照。
#[derive(Debug, Clone, Serialize)]
pub struct ModelCatalog {
    pub providers: Vec<ModelProviderCatalog>,
    pub models: Vec<ModelCatalogItem>,
    pub capabilities: Vec<String>,
}

/// 前端展示用的模型提供器目录条目。
#[derive(Debug, Clone, Serialize)]
pub struct ModelProviderCatalog {
    pub id: String,
    pub name: String,
    pub default_api_base: String,
    pub chat_model_list_url: String,
    pub tts_model_list_url: String,
    pub enabled: bool,
    pub notes: String,
    pub supports_balance_check: bool,
    pub capabilities: Vec<String>,
    pub model_list_auth: String,
    pub allow_custom_base: bool,
    pub connection_validation: String,
    pub status: String,
    /// 受保护的 `config.toml` 中是否已配置密钥。
    pub api_key_configured: bool,
    /// Provider Profile 的内部密钥副本；目录接口不向前端泄露明文。
    #[serde(skip_serializing)]
    pub api_key: String,
}

/// 前端展示用的模型目录条目。
#[derive(Debug, Clone, Serialize)]
pub struct ModelCatalogItem {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub model: String,
    pub default_api_base: String,
    pub enabled: bool,
    pub notes: String,
    pub tags: Vec<String>,
    pub functions: Vec<String>,
    pub capabilities: Vec<String>,
    pub context_window: u64,
    pub default_max_output_tokens: u32,
    pub supports_usage: bool,
    pub supports_cached_tokens: bool,
    pub supports_reasoning_tokens: bool,
    pub tokenizer_family: String,
}

/// 创建或编辑模型目录时使用的非敏感字段。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelCatalogModelDraft {
    pub provider_id: String,
    pub model: String,
    pub name: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_chat_functions")]
    pub functions: Vec<String>,
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_max_output_tokens")]
    pub default_max_output_tokens: u32,
    #[serde(default = "default_true")]
    pub supports_usage: bool,
    #[serde(default)]
    pub supports_cached_tokens: bool,
    #[serde(default = "default_true")]
    pub supports_reasoning_tokens: bool,
    #[serde(default = "default_tokenizer_family")]
    pub tokenizer_family: String,
}

fn default_chat_functions() -> Vec<String> {
    vec!["chat".to_string()]
}

fn default_context_window() -> u64 {
    DEFAULT_MODEL_CAPABILITY_DEFAULTS.context_window
}

fn default_max_output_tokens() -> u32 {
    DEFAULT_MODEL_CAPABILITY_DEFAULTS.default_max_output_tokens
}

fn default_true() -> bool {
    true
}

fn default_tokenizer_family() -> String {
    DEFAULT_MODEL_CAPABILITY_DEFAULTS
        .tokenizer_family
        .to_string()
}

pub(crate) fn validate_model_draft(
    mut draft: ModelCatalogModelDraft,
) -> Result<ModelCatalogModelDraft, ModelCatalogError> {
    draft.provider_id = validate_identifier("供应商", &draft.provider_id, 128)?;
    draft.model = validate_identifier("模型 ID", &draft.model, 256)?;
    draft.name = validate_text("模型名称", &draft.name, 128, false)?;
    draft.notes = validate_text("模型说明", &draft.notes, 1000, true)?;
    draft.tokenizer_family = validate_identifier("Tokenizer", &draft.tokenizer_family, 64)?;
    draft.tags = normalize_labels(draft.tags, "模型标签")?;
    draft.functions = normalize_labels(draft.functions, "模型功能")?;
    if draft.functions != ["chat"] {
        return Err(ModelCatalogError::Validation(
            "当前模型目录只允许创建对话模型。".to_string(),
        ));
    }
    if draft.context_window == 0 || draft.context_window > 10_000_000 {
        return Err(ModelCatalogError::Validation(
            "上下文窗口必须在 1 到 10000000 之间。".to_string(),
        ));
    }
    if draft.default_max_output_tokens == 0
        || u64::from(draft.default_max_output_tokens) > draft.context_window
    {
        return Err(ModelCatalogError::Validation(
            "默认最大输出必须大于 0 且不能超过上下文窗口。".to_string(),
        ));
    }
    Ok(draft)
}

fn validate_identifier(
    label: &str,
    value: &str,
    max_len: usize,
) -> Result<String, ModelCatalogError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_len || value.chars().any(char::is_control) {
        return Err(ModelCatalogError::Validation(format!(
            "{label}必须是 1 到 {max_len} 个不含控制字符的字符。"
        )));
    }
    Ok(value.to_string())
}

fn validate_text(
    label: &str,
    value: &str,
    max_len: usize,
    allow_empty: bool,
) -> Result<String, ModelCatalogError> {
    let value = value.trim();
    if (!allow_empty && value.is_empty())
        || value.chars().count() > max_len
        || value.chars().any(char::is_control)
    {
        return Err(ModelCatalogError::Validation(format!(
            "{label}{}且不能超过 {max_len} 个字符。",
            if allow_empty {
                "不能包含控制字符"
            } else {
                "不能为空"
            }
        )));
    }
    Ok(value.to_string())
}

fn normalize_labels(values: Vec<String>, label: &str) -> Result<Vec<String>, ModelCatalogError> {
    let mut normalized = values
        .into_iter()
        .map(|value| {
            if value.contains(',') {
                return Err(ModelCatalogError::Validation(format!(
                    "{label}不能包含逗号。"
                )));
            }
            validate_identifier(label, &value, 64)
        })
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

pub(crate) fn merge_labels(functions: &[String], tags: &[String]) -> Vec<String> {
    let mut values = BTreeSet::new();
    values.extend(functions.iter().cloned());
    values.extend(tags.iter().cloned());
    values.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{ModelCatalogModelDraft, ModelProviderCatalog, validate_model_draft};

    #[test]
    fn model_draft_validation_normalizes_labels() {
        let draft = validate_model_draft(ModelCatalogModelDraft {
            provider_id: " deepseek ".to_string(),
            model: " deepseek-chat ".to_string(),
            name: " DeepSeek Chat ".to_string(),
            notes: String::new(),
            tags: vec![
                "tool".to_string(),
                "reasoning".to_string(),
                "tool".to_string(),
            ],
            functions: vec!["chat".to_string()],
            context_window: 128_000,
            default_max_output_tokens: 8_192,
            supports_usage: true,
            supports_cached_tokens: true,
            supports_reasoning_tokens: true,
            tokenizer_family: "rough_estimate".to_string(),
        })
        .expect("当前格式的模型草稿应通过校验");

        assert_eq!(draft.provider_id, "deepseek");
        assert_eq!(draft.tags, ["reasoning", "tool"]);
    }

    #[test]
    fn provider_catalog_serialization_never_exposes_api_key() {
        let provider = ModelProviderCatalog {
            id: "deepseek".to_string(),
            name: "DeepSeek".to_string(),
            default_api_base: "https://api.deepseek.com".to_string(),
            chat_model_list_url: String::new(),
            tts_model_list_url: String::new(),
            enabled: true,
            notes: String::new(),
            supports_balance_check: true,
            capabilities: vec!["chat".to_string()],
            model_list_auth: "required".to_string(),
            allow_custom_base: false,
            connection_validation: "model_list".to_string(),
            status: "supported".to_string(),
            api_key_configured: true,
            api_key: "must-not-leak".to_string(),
        };

        let json = serde_json::to_string(&provider).expect("目录应可序列化");
        assert!(!json.contains("must-not-leak"));
    }
}
