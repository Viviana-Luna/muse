//! `config.toml` 中的完整 Provider Profile 与活动模型选择。
//!
//! 供应商、端点、API Key、模型列表和模型默认参数必须作为一个配置域原子发布。
//! 运行时使用的 `ModelsConfig` 只由这里解析生成，不再单独持久化。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::catalog::{
    ModelCatalog, ModelCatalogError, ModelCatalogItem, ModelCatalogModelDraft,
    ModelProviderCatalog, merge_labels, validate_model_draft,
};
use crate::model::config::{
    LlmConfig, ModelsConfig, SpeechRecognitionConfig, TtsConfig, VoiceInputConfig,
};
use crate::model::profile::{
    DEEPSEEK_PROVIDER_PROFILE, DEFAULT_MODEL_CAPABILITY_DEFAULTS,
    VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL, VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE,
};
use crate::model::vendor::provider_support_capabilities;

const CHAT_MODALITY: &str = "chat";
const TTS_MODALITY: &str = "tts";
const ASR_MODALITY: &str = "asr";
const AUDIO_UNDERSTANDING_MODALITY: &str = "audio_understanding";

/// 已校验、可直接冻结到 Turn 的聊天模型配置与公开能力事实。
#[derive(Debug, Clone)]
pub struct ResolvedChatModel {
    pub config: LlmConfig,
    pub catalog: ModelCatalogItem,
}

/// Persona 或全局活动模型引用无法用于聊天时的稳定诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatModelReferenceError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ChatModelReferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}：{}", self.code, self.message)
    }
}

impl std::error::Error for ChatModelReferenceError {}

/// 一个供应商下的模型声明和默认运行参数。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModelConfig {
    #[serde(default)]
    pub display_name: String,
    #[serde(default = "default_chat_modality")]
    pub modality: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default)]
    pub voice_id: String,
    #[serde(default = "default_voice_speed")]
    pub speed: f32,
    #[serde(default)]
    pub response_format: String,
    #[serde(default)]
    pub language: String,
}

impl std::fmt::Debug for ProviderModelConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderModelConfig")
            .field("display_name", &self.display_name)
            .field("modality", &self.modality)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl ProviderModelConfig {
    fn from_catalog_draft(draft: &ModelCatalogModelDraft) -> Self {
        Self {
            display_name: draft.name.clone(),
            modality: draft
                .functions
                .first()
                .cloned()
                .unwrap_or_else(default_chat_modality),
            enabled: true,
            notes: draft.notes.clone(),
            tags: draft.tags.clone(),
            functions: draft.functions.clone(),
            context_window: draft.context_window,
            default_max_output_tokens: draft.default_max_output_tokens,
            supports_usage: draft.supports_usage,
            supports_cached_tokens: draft.supports_cached_tokens,
            supports_reasoning_tokens: draft.supports_reasoning_tokens,
            tokenizer_family: draft.tokenizer_family.clone(),
            temperature: default_temperature(),
            voice_id: String::new(),
            speed: default_voice_speed(),
            response_format: String::new(),
            language: String::new(),
        }
    }

    fn catalog_item(
        &self,
        provider_id: &str,
        provider: &ProviderProfileConfig,
        model_id: &str,
    ) -> ModelCatalogItem {
        let functions = if self.functions.is_empty() {
            vec![self.modality.clone()]
        } else {
            self.functions.clone()
        };
        ModelCatalogItem {
            id: format!("{provider_id}:{model_id}"),
            provider_id: provider_id.to_string(),
            name: if self.display_name.trim().is_empty() {
                model_id.to_string()
            } else {
                self.display_name.clone()
            },
            model: model_id.to_string(),
            default_api_base: provider.base_url.clone(),
            enabled: self.enabled,
            notes: self.notes.clone(),
            tags: self.tags.clone(),
            capabilities: merge_labels(&functions, &self.tags),
            functions,
            context_window: self.context_window,
            default_max_output_tokens: self.default_max_output_tokens,
            supports_usage: self.supports_usage,
            supports_cached_tokens: self.supports_cached_tokens,
            supports_reasoning_tokens: self.supports_reasoning_tokens,
            tokenizer_family: self.tokenizer_family.clone(),
        }
    }
}

impl Default for ProviderModelConfig {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            modality: default_chat_modality(),
            enabled: true,
            notes: String::new(),
            tags: Vec::new(),
            functions: default_chat_functions(),
            context_window: default_context_window(),
            default_max_output_tokens: default_max_output_tokens(),
            supports_usage: true,
            supports_cached_tokens: false,
            supports_reasoning_tokens: true,
            tokenizer_family: default_tokenizer_family(),
            temperature: default_temperature(),
            voice_id: String::new(),
            speed: default_voice_speed(),
            response_format: String::new(),
            language: String::new(),
        }
    }
}

/// 完整供应商配置。API Key 只允许由受保护的 `config.toml` 持久化。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderProfileConfig {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub chat_model_list_url: String,
    #[serde(default)]
    pub tts_model_list_url: String,
    #[serde(default)]
    pub models: BTreeMap<String, ProviderModelConfig>,
}

impl std::fmt::Debug for ProviderProfileConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderProfileConfig")
            .field("kind", &self.kind)
            .field("display_name", &self.display_name)
            .field("base_url", &self.base_url)
            .field("api_key_configured", &self.api_key_configured())
            .field("enabled", &self.enabled)
            .field("models", &self.models)
            .finish()
    }
}

impl ProviderProfileConfig {
    pub fn api_key_configured(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    }

    fn catalog_provider(&self, id: &str) -> ModelProviderCatalog {
        let support = provider_support_capabilities(&self.kind, &self.base_url);
        let requires_chat_probe = self.kind == VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE.id;
        let supported = chat_provider_runtime_supported(id);
        let mut capabilities = self
            .models
            .values()
            .filter(|model| model.enabled)
            .flat_map(|model| model.functions.iter().cloned())
            .collect::<Vec<_>>();
        capabilities.sort();
        capabilities.dedup();
        ModelProviderCatalog {
            id: id.to_string(),
            name: if self.display_name.trim().is_empty() {
                id.to_string()
            } else {
                self.display_name.clone()
            },
            default_api_base: self.base_url.clone(),
            chat_model_list_url: self.chat_model_list_url.clone(),
            tts_model_list_url: self.tts_model_list_url.clone(),
            enabled: self.enabled,
            notes: self.notes.clone(),
            supports_balance_check: support.supports_balance_check,
            capabilities,
            model_list_auth:
                if requires_chat_probe || !self.chat_model_list_url.trim().is_empty() {
                    "required"
                } else {
                    "none"
                }
                .to_string(),
            allow_custom_base: false,
            connection_validation: if requires_chat_probe {
                "chat_probe"
            } else if self.chat_model_list_url.trim().is_empty() {
                "chat_request"
            } else {
                "model_list"
            }
            .to_string(),
            status: if supported {
                "supported"
            } else {
                "legacy_unsupported"
            }
            .to_string(),
            api_key_configured: self.api_key_configured(),
            credential_diagnostic: None,
            api_key: self.api_key.clone().unwrap_or_default(),
            credential_account: String::new(),
            credential_identity: String::new(),
        }
    }
}

/// 一个运行槽位选择的 Provider 与模型；`enabled` 仅对可选能力生效。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveModelSelection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

/// 聊天、TTS、ASR 和音频理解的活动选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveModelsConfig {
    #[serde(default)]
    pub chat: ActiveModelSelection,
    #[serde(default)]
    pub tts: ActiveModelSelection,
    #[serde(default)]
    pub asr: ActiveModelSelection,
    #[serde(default)]
    pub audio_understanding: ActiveModelSelection,
    #[serde(default = "default_voice_input_mode")]
    pub voice_input_mode: String,
}

impl Default for ActiveModelsConfig {
    fn default() -> Self {
        Self {
            chat: ActiveModelSelection::default(),
            tts: ActiveModelSelection::default(),
            asr: ActiveModelSelection::default(),
            audio_understanding: ActiveModelSelection::default(),
            voice_input_mode: default_voice_input_mode(),
        }
    }
}

/// `config.toml` 模型领域的强类型快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProfileConfig {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderProfileConfig>,
    #[serde(default)]
    pub active_models: ActiveModelsConfig,
}

impl Default for ModelProfileConfig {
    fn default() -> Self {
        Self {
            providers: builtin_provider_profiles(),
            active_models: ActiveModelsConfig::default(),
        }
    }
}

impl ModelProfileConfig {
    /// 从完整 Provider Profile 解析当前运行时使用的兼容配置结构。
    pub fn resolve_runtime(&self) -> ModelsConfig {
        ModelsConfig {
            chat: self.resolve_chat(),
            tts: self.resolve_tts(),
            speech_recognition: self.resolve_asr(),
            audio_understanding: self.resolve_audio_understanding(),
            voice_input: VoiceInputConfig {
                mode: self.active_models.voice_input_mode.clone(),
            },
            mcp_servers: BTreeMap::new(),
            secret_bindings: Default::default(),
        }
    }

    pub fn catalog(&self) -> ModelCatalog {
        let providers = self
            .providers
            .iter()
            .map(|(id, provider)| provider.catalog_provider(id))
            .collect::<Vec<_>>();
        let models = self
            .providers
            .iter()
            .flat_map(|(provider_id, provider)| {
                provider
                    .models
                    .iter()
                    .filter(|(_, model)| model.enabled)
                    .map(|(model_id, model)| model.catalog_item(provider_id, provider, model_id))
            })
            .collect::<Vec<_>>();
        let mut capabilities = models
            .iter()
            .flat_map(|model| model.capabilities.iter().cloned())
            .collect::<Vec<_>>();
        capabilities.sort();
        capabilities.dedup();
        ModelCatalog {
            providers,
            models,
            capabilities,
        }
    }

    pub fn provider(&self, provider_id: &str) -> Option<ModelProviderCatalog> {
        self.providers
            .get(provider_id)
            .filter(|provider| provider.enabled)
            .map(|provider| provider.catalog_provider(provider_id))
    }

    /// 返回设置中心使用的供应商事实，不把关闭状态误判为供应商不存在。
    pub fn managed_provider(&self, provider_id: &str) -> Option<ModelProviderCatalog> {
        self.providers
            .get(provider_id)
            .map(|provider| provider.catalog_provider(provider_id))
    }

    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<ModelCatalogItem> {
        let provider = self.providers.get(provider_id)?;
        if !provider.enabled {
            return None;
        }
        provider
            .models
            .get(model_id)
            .filter(|model| model.enabled)
            .map(|model| model.catalog_item(provider_id, provider, model_id))
    }

    fn managed_model(&self, provider_id: &str, model_id: &str) -> Option<ModelCatalogItem> {
        let provider = self.providers.get(provider_id)?;
        provider
            .models
            .get(model_id)
            .filter(|model| model.enabled)
            .map(|model| model.catalog_item(provider_id, provider, model_id))
    }

    /// 解析全局活动聊天模型，供 Turn 回退路径与 Persona 引用使用同一套可用性门禁。
    pub fn resolve_active_chat_model(&self) -> Result<ResolvedChatModel, ChatModelReferenceError> {
        let selection = &self.active_models.chat;
        if selection.provider.trim().is_empty() || selection.model.trim().is_empty() {
            return Err(chat_reference_error(
                "global_chat_model_missing",
                "尚未配置全局活动聊天模型。",
            ));
        }
        self.resolve_chat_model_reference(&selection.provider, &selection.model)
    }

    /// 把一个非敏感 Provider/模型引用解析为完整运行配置；不会改写活动模型。
    pub fn resolve_chat_model_reference(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ResolvedChatModel, ChatModelReferenceError> {
        let provider_id = provider_id.trim();
        let model_id = model_id.trim();
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            chat_reference_error(
                "persona_model_provider_missing",
                format!("供应商 `{provider_id}` 不存在。"),
            )
        })?;
        if !provider.enabled {
            return Err(chat_reference_error(
                "persona_model_provider_disabled",
                format!("供应商 `{provider_id}` 已停用。"),
            ));
        }
        if !chat_provider_runtime_supported(provider_id) {
            return Err(chat_reference_error(
                "persona_model_provider_unsupported",
                format!("供应商 `{provider_id}` 暂不支持聊天运行时。"),
            ));
        }
        if !provider.api_key_configured() {
            return Err(chat_reference_error(
                "persona_model_api_key_missing",
                format!("供应商 `{provider_id}` 尚未配置 API Key。"),
            ));
        }
        let model = provider.models.get(model_id).ok_or_else(|| {
            chat_reference_error(
                "persona_model_missing",
                format!("模型 `{provider_id}:{model_id}` 不存在。"),
            )
        })?;
        if !model.enabled {
            return Err(chat_reference_error(
                "persona_model_disabled",
                format!("模型 `{provider_id}:{model_id}` 已停用。"),
            ));
        }
        let supports_chat = model.modality == CHAT_MODALITY
            || model
                .functions
                .iter()
                .any(|function| function == CHAT_MODALITY);
        if !supports_chat {
            return Err(chat_reference_error(
                "persona_model_not_chat_capable",
                format!("模型 `{provider_id}:{model_id}` 未声明聊天能力。"),
            ));
        }

        Ok(ResolvedChatModel {
            config: LlmConfig {
                provider: provider_id.to_string(),
                api_base: provider.base_url.clone(),
                api_key: provider.api_key.clone(),
                api_protocol: "chat_completions".to_string(),
                model: model_id.to_string(),
                max_tokens: model.default_max_output_tokens,
                temperature: model.temperature,
            },
            catalog: model.catalog_item(provider_id, provider, model_id),
        })
    }

    pub fn create_model(
        &mut self,
        draft: ModelCatalogModelDraft,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        let draft = validate_model_draft(draft)?;
        let provider = self.providers.get_mut(&draft.provider_id).ok_or_else(|| {
            ModelCatalogError::Validation("创建模型时只能选择现有供应商。".to_string())
        })?;
        if provider
            .models
            .get(&draft.model)
            .is_some_and(|model| model.enabled)
        {
            return Err(ModelCatalogError::Conflict(format!(
                "模型 `{}` 已存在，请直接编辑该模型。",
                draft.model
            )));
        }
        provider.models.insert(
            draft.model.clone(),
            ProviderModelConfig::from_catalog_draft(&draft),
        );
        self.managed_model(&draft.provider_id, &draft.model)
            .ok_or_else(|| ModelCatalogError::NotFound("模型创建后无法读取。".to_string()))
    }

    pub fn update_model(
        &mut self,
        draft: ModelCatalogModelDraft,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        let draft = validate_model_draft(draft)?;
        let provider = self.providers.get_mut(&draft.provider_id).ok_or_else(|| {
            ModelCatalogError::Validation("编辑模型时只能选择现有供应商。".to_string())
        })?;
        let model = provider
            .models
            .get_mut(&draft.model)
            .filter(|model| model.enabled)
            .ok_or_else(|| {
                ModelCatalogError::NotFound(format!("模型 `{}` 不存在或已删除。", draft.model))
            })?;
        let temperature = model.temperature;
        let voice_id = model.voice_id.clone();
        let speed = model.speed;
        let response_format = model.response_format.clone();
        let language = model.language.clone();
        *model = ProviderModelConfig::from_catalog_draft(&draft);
        model.temperature = temperature;
        model.voice_id = voice_id;
        model.speed = speed;
        model.response_format = response_format;
        model.language = language;
        self.managed_model(&draft.provider_id, &draft.model)
            .ok_or_else(|| ModelCatalogError::NotFound("模型编辑后无法读取。".to_string()))
    }

    pub fn disable_model(
        &mut self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), ModelCatalogError> {
        let model = self
            .providers
            .get_mut(provider_id)
            .and_then(|provider| provider.models.get_mut(model_id))
            .filter(|model| model.enabled)
            .ok_or_else(|| {
                ModelCatalogError::NotFound(format!("模型 `{model_id}` 不存在或已删除。"))
            })?;
        model.enabled = false;
        Ok(())
    }

    pub fn ensure_runtime_model(
        &mut self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        if let Some(model) = self.model(provider_id, model_id) {
            return Ok(model);
        }
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ModelCatalogError::NotFound("当前供应商不存在。".to_string()))?;
        let defaults = crate::model::profile::provider_profile_for_identity(
            &provider.kind,
            &provider.base_url,
        )
        .map(|profile| profile.model_defaults)
        .unwrap_or(DEFAULT_MODEL_CAPABILITY_DEFAULTS);
        self.create_model(ModelCatalogModelDraft {
            provider_id: provider_id.to_string(),
            model: model_id.to_string(),
            name: model_id.to_string(),
            notes: "从当前运行配置自动补录，可在模型配置页完善名称与能力。".to_string(),
            tags: vec!["reasoning".to_string(), "tool".to_string()],
            functions: default_chat_functions(),
            context_window: defaults.context_window,
            default_max_output_tokens: defaults.default_max_output_tokens,
            supports_usage: defaults.supports_usage,
            supports_cached_tokens: defaults.supports_cached_tokens,
            supports_reasoning_tokens: defaults.supports_reasoning_tokens,
            tokenizer_family: defaults.tokenizer_family.to_string(),
        })
    }

    pub fn set_provider_api_key(
        &mut self,
        provider_id: &str,
        api_key: Option<String>,
    ) -> Result<bool, ModelCatalogError> {
        let provider = self
            .providers
            .get_mut(provider_id)
            .ok_or_else(|| ModelCatalogError::NotFound("当前供应商不存在。".to_string()))?;
        provider.api_key = api_key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Ok(provider.api_key_configured())
    }

    pub fn set_provider_enabled(
        &mut self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<ModelProviderCatalog, ModelCatalogError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ModelCatalogError::NotFound("当前供应商不存在。".to_string()))?;
        if provider.enabled == enabled {
            return Ok(provider.catalog_provider(provider_id));
        }

        if enabled {
            if !chat_provider_runtime_supported(provider_id) {
                return Err(ModelCatalogError::Validation(
                    "provider_runtime_unsupported：当前供应商暂不支持聊天运行时。".to_string(),
                ));
            }
            if !provider.api_key_configured() {
                return Err(ModelCatalogError::Validation(
                    "provider_api_key_required：请先保存该供应商的 API Key。".to_string(),
                ));
            }
            let has_chat_model = provider.models.values().any(|model| {
                model.enabled
                    && (model.modality == CHAT_MODALITY
                        || model
                            .functions
                            .iter()
                            .any(|function| function == CHAT_MODALITY))
            });
            if !has_chat_model {
                return Err(ModelCatalogError::Validation(
                    "provider_chat_model_required：请先为该供应商配置至少一个聊天模型。"
                        .to_string(),
                ));
            }
        } else if self.active_models.chat.provider == provider_id {
            self.active_models.chat = ActiveModelSelection::default();
        }

        let provider = self
            .providers
            .get_mut(provider_id)
            .expect("供应商已在前置校验中确认存在");
        provider.enabled = enabled;
        Ok(provider.catalog_provider(provider_id))
    }

    pub fn set_active_chat(
        &mut self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), ModelCatalogError> {
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| ModelCatalogError::NotFound("当前供应商不存在或已停用。".to_string()))?;
        if !provider.api_key_configured {
            return Err(ModelCatalogError::Validation(
                "provider_api_key_required：请先在模型配置页保存该供应商的 API Key。".to_string(),
            ));
        }
        self.model(provider_id, model_id)
            .ok_or_else(|| ModelCatalogError::NotFound("当前模型不存在或已删除。".to_string()))?;
        self.active_models.chat = ActiveModelSelection {
            enabled: true,
            provider: provider_id.to_string(),
            model: model_id.to_string(),
        };
        Ok(())
    }

    /// 把兼容 API 的四段运行配置合并回 Provider Profile。
    pub fn apply_runtime_config(&mut self, config: &ModelsConfig) -> Result<(), ModelCatalogError> {
        validate_shared_provider_identity(config)?;
        self.apply_llm_selection(&config.chat, CHAT_MODALITY, true)?;
        self.apply_tts_selection(&config.tts)?;
        self.apply_asr_selection(&config.speech_recognition)?;
        self.apply_llm_selection(
            &config.audio_understanding,
            AUDIO_UNDERSTANDING_MODALITY,
            !config.audio_understanding.provider.trim().is_empty(),
        )?;
        self.active_models.voice_input_mode = config.voice_input.mode.clone();
        Ok(())
    }

    fn resolve_chat(&self) -> LlmConfig {
        let Some((provider_id, provider, model_id, model)) =
            self.resolve_selection(&self.active_models.chat)
        else {
            return LlmConfig::default();
        };
        LlmConfig {
            provider: provider_id.to_string(),
            api_base: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            api_protocol: "chat_completions".to_string(),
            model: model_id.to_string(),
            max_tokens: model.default_max_output_tokens,
            temperature: model.temperature,
        }
    }

    fn resolve_tts(&self) -> TtsConfig {
        let Some((provider_id, provider, model_id, model)) =
            self.resolve_selection(&self.active_models.tts)
        else {
            return TtsConfig::default();
        };
        TtsConfig {
            enabled: self.active_models.tts.enabled,
            provider: provider_id.to_string(),
            api_base: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            model: model_id.to_string(),
            voice_id: model.voice_id.clone(),
            speed: model.speed,
            response_format: model.response_format.clone(),
        }
    }

    fn resolve_asr(&self) -> SpeechRecognitionConfig {
        let Some((provider_id, provider, model_id, model)) =
            self.resolve_selection(&self.active_models.asr)
        else {
            return SpeechRecognitionConfig::default();
        };
        SpeechRecognitionConfig {
            enabled: self.active_models.asr.enabled,
            provider: provider_id.to_string(),
            api_base: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            model: model_id.to_string(),
            language: model.language.clone(),
            response_format: model.response_format.clone(),
        }
    }

    fn resolve_audio_understanding(&self) -> LlmConfig {
        let Some((provider_id, provider, model_id, model)) =
            self.resolve_selection(&self.active_models.audio_understanding)
        else {
            return LlmConfig::default();
        };
        LlmConfig {
            provider: provider_id.to_string(),
            api_base: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            api_protocol: "chat_completions".to_string(),
            model: model_id.to_string(),
            max_tokens: model.default_max_output_tokens,
            temperature: model.temperature,
        }
    }

    fn resolve_selection<'a>(
        &'a self,
        selection: &'a ActiveModelSelection,
    ) -> Option<(
        &'a str,
        &'a ProviderProfileConfig,
        &'a str,
        &'a ProviderModelConfig,
    )> {
        let provider = self
            .providers
            .get(&selection.provider)
            .filter(|provider| provider.enabled)?;
        let model = provider
            .models
            .get(&selection.model)
            .filter(|model| model.enabled)?;
        Some((
            selection.provider.as_str(),
            provider,
            selection.model.as_str(),
            model,
        ))
    }

    fn apply_llm_selection(
        &mut self,
        config: &LlmConfig,
        modality: &str,
        enabled: bool,
    ) -> Result<(), ModelCatalogError> {
        if config.provider.trim().is_empty() || config.model.trim().is_empty() {
            let selection = selection_mut(&mut self.active_models, modality);
            *selection = ActiveModelSelection::default();
            return Ok(());
        }
        let provider_id = config.provider.trim();
        let model_id = config.model.trim();
        let provider = ensure_provider_profile(
            &mut self.providers,
            provider_id,
            &config.api_base,
            config.api_key.clone(),
        );
        let model = provider.models.entry(model_id.to_string()).or_default();
        model.display_name = if model.display_name.trim().is_empty() {
            model_id.to_string()
        } else {
            model.display_name.clone()
        };
        model.modality = modality.to_string();
        model.enabled = true;
        model.functions = vec![modality.to_string()];
        model.default_max_output_tokens = config.max_tokens;
        model.temperature = config.temperature;
        *selection_mut(&mut self.active_models, modality) = ActiveModelSelection {
            enabled,
            provider: provider_id.to_string(),
            model: model_id.to_string(),
        };
        Ok(())
    }

    fn apply_tts_selection(&mut self, config: &TtsConfig) -> Result<(), ModelCatalogError> {
        if config.provider.trim().is_empty() || config.model.trim().is_empty() {
            self.active_models.tts = ActiveModelSelection::default();
            return Ok(());
        }
        let provider_id = config.provider.trim();
        let model_id = config.model.trim();
        let provider = ensure_provider_profile(
            &mut self.providers,
            provider_id,
            &config.api_base,
            config.api_key.clone(),
        );
        let model = provider.models.entry(model_id.to_string()).or_default();
        model.display_name = if model.display_name.trim().is_empty() {
            model_id.to_string()
        } else {
            model.display_name.clone()
        };
        model.modality = TTS_MODALITY.to_string();
        model.enabled = true;
        model.functions = vec![TTS_MODALITY.to_string()];
        model.voice_id = config.voice_id.clone();
        model.speed = config.speed;
        model.response_format = config.response_format.clone();
        self.active_models.tts = ActiveModelSelection {
            enabled: config.enabled,
            provider: provider_id.to_string(),
            model: model_id.to_string(),
        };
        Ok(())
    }

    fn apply_asr_selection(
        &mut self,
        config: &SpeechRecognitionConfig,
    ) -> Result<(), ModelCatalogError> {
        if config.provider.trim().is_empty() || config.model.trim().is_empty() {
            self.active_models.asr = ActiveModelSelection::default();
            return Ok(());
        }
        let provider_id = config.provider.trim();
        let model_id = config.model.trim();
        let provider = ensure_provider_profile(
            &mut self.providers,
            provider_id,
            &config.api_base,
            config.api_key.clone(),
        );
        let model = provider.models.entry(model_id.to_string()).or_default();
        model.display_name = if model.display_name.trim().is_empty() {
            model_id.to_string()
        } else {
            model.display_name.clone()
        };
        model.modality = ASR_MODALITY.to_string();
        model.enabled = true;
        model.functions = vec![ASR_MODALITY.to_string()];
        model.language = config.language.clone();
        model.response_format = config.response_format.clone();
        self.active_models.asr = ActiveModelSelection {
            enabled: config.enabled,
            provider: provider_id.to_string(),
            model: model_id.to_string(),
        };
        Ok(())
    }
}

fn chat_reference_error(
    code: impl Into<String>,
    message: impl Into<String>,
) -> ChatModelReferenceError {
    ChatModelReferenceError {
        code: code.into(),
        message: message.into(),
    }
}

fn validate_shared_provider_identity(config: &ModelsConfig) -> Result<(), ModelCatalogError> {
    let mut identities = BTreeMap::<String, (String, Option<String>)>::new();
    for (provider, base_url, api_key) in [
        (
            config.chat.provider.as_str(),
            config.chat.api_base.as_str(),
            config.chat.api_key.as_ref(),
        ),
        (
            config.tts.provider.as_str(),
            config.tts.api_base.as_str(),
            config.tts.api_key.as_ref(),
        ),
        (
            config.speech_recognition.provider.as_str(),
            config.speech_recognition.api_base.as_str(),
            config.speech_recognition.api_key.as_ref(),
        ),
        (
            config.audio_understanding.provider.as_str(),
            config.audio_understanding.api_base.as_str(),
            config.audio_understanding.api_key.as_ref(),
        ),
    ] {
        let provider = provider.trim();
        if provider.is_empty() {
            continue;
        }
        let identity = (
            base_url.trim().trim_end_matches('/').to_string(),
            api_key
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        );
        if let Some(previous) = identities.get(provider)
            && previous != &identity
        {
            return Err(ModelCatalogError::Validation(format!(
                "同一供应商 `{provider}` 在不同模型用途中的 base_url 或 API Key 不一致，请统一后再保存。"
            )));
        }
        identities.insert(provider.to_string(), identity);
    }
    Ok(())
}

fn ensure_provider_profile<'a>(
    providers: &'a mut BTreeMap<String, ProviderProfileConfig>,
    provider_id: &str,
    base_url: &str,
    api_key: Option<String>,
) -> &'a mut ProviderProfileConfig {
    let provider =
        providers
            .entry(provider_id.to_string())
            .or_insert_with(|| ProviderProfileConfig {
                kind: provider_id.to_string(),
                display_name: provider_id.to_string(),
                base_url: base_url.trim().trim_end_matches('/').to_string(),
                api_key: None,
                enabled: true,
                notes: String::new(),
                chat_model_list_url: String::new(),
                tts_model_list_url: String::new(),
                models: BTreeMap::new(),
            });
    provider.base_url = base_url.trim().trim_end_matches('/').to_string();
    provider.api_key = api_key
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    provider
}

fn selection_mut<'a>(
    active: &'a mut ActiveModelsConfig,
    modality: &str,
) -> &'a mut ActiveModelSelection {
    match modality {
        CHAT_MODALITY => &mut active.chat,
        TTS_MODALITY => &mut active.tts,
        ASR_MODALITY => &mut active.asr,
        AUDIO_UNDERSTANDING_MODALITY => &mut active.audio_understanding,
        _ => &mut active.chat,
    }
}

fn builtin_provider_profiles() -> BTreeMap<String, ProviderProfileConfig> {
    let mut providers = BTreeMap::new();
    for profile in [
        DEEPSEEK_PROVIDER_PROFILE,
        VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE,
    ] {
        let models = if profile.id == DEEPSEEK_PROVIDER_PROFILE.id {
            [
                (
                    "deepseek-v4-flash",
                    "DeepSeek V4 Flash",
                    "DeepSeek 当前推荐的快速对话模型。",
                ),
                (
                    "deepseek-v4-pro",
                    "DeepSeek V4 Pro",
                    "DeepSeek 当前推荐的高质量对话模型。",
                ),
            ]
            .into_iter()
            .collect::<Vec<_>>()
        } else {
            vec![(
                VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL,
                "Doubao Seed 2.0 Pro",
                "火山方舟 Agent Plan 套餐模型。",
            )]
        };
        let models = models
            .into_iter()
            .map(|(id, name, notes)| {
                (
                    id.to_string(),
                    ProviderModelConfig {
                        display_name: name.to_string(),
                        notes: notes.to_string(),
                        tags: vec!["reasoning".to_string(), "tool".to_string()],
                        functions: default_chat_functions(),
                        context_window: profile.model_defaults.context_window,
                        default_max_output_tokens: profile.model_defaults.default_max_output_tokens,
                        supports_usage: profile.model_defaults.supports_usage,
                        supports_cached_tokens: profile.model_defaults.supports_cached_tokens,
                        supports_reasoning_tokens: profile.model_defaults.supports_reasoning_tokens,
                        tokenizer_family: profile.model_defaults.tokenizer_family.to_string(),
                        ..ProviderModelConfig::default()
                    },
                )
            })
            .collect();
        providers.insert(
            profile.id.to_string(),
            ProviderProfileConfig {
                kind: profile.id.to_string(),
                display_name: profile.name.to_string(),
                base_url: profile.default_api_base.to_string(),
                api_key: None,
                enabled: false,
                notes: profile.notes.to_string(),
                chat_model_list_url: profile.chat_model_list_url.to_string(),
                tts_model_list_url: String::new(),
                models,
            },
        );
    }
    providers
}

fn default_chat_modality() -> String {
    CHAT_MODALITY.to_string()
}

fn default_chat_functions() -> Vec<String> {
    vec![CHAT_MODALITY.to_string()]
}

fn default_context_window() -> u64 {
    DEFAULT_MODEL_CAPABILITY_DEFAULTS.context_window
}

fn default_max_output_tokens() -> u32 {
    DEFAULT_MODEL_CAPABILITY_DEFAULTS.default_max_output_tokens
}

fn default_tokenizer_family() -> String {
    DEFAULT_MODEL_CAPABILITY_DEFAULTS
        .tokenizer_family
        .to_string()
}

fn default_temperature() -> f64 {
    0.7
}

fn default_voice_speed() -> f32 {
    1.0
}

fn default_voice_input_mode() -> String {
    "speech_text".to_string()
}

fn default_true() -> bool {
    true
}

fn chat_provider_runtime_supported(provider_id: &str) -> bool {
    matches!(provider_id, "deepseek" | "volcengine_agent_plan")
}

#[cfg(test)]
mod tests {
    use super::{ModelProfileConfig, ProviderProfileConfig};

    fn enable_provider(profiles: &mut ModelProfileConfig, provider_id: &str, secret: &str) {
        profiles
            .set_provider_api_key(provider_id, Some(secret.to_string()))
            .expect("应保存 Provider Key");
        profiles
            .set_provider_enabled(provider_id, true)
            .expect("凭据和聊天模型齐全时应能开启供应商");
    }

    #[test]
    fn missing_provider_enabled_field_defaults_to_disabled() {
        let provider: ProviderProfileConfig = toml_edit::de::from_str(
            r#"
kind = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
"#,
        )
        .expect("缺少 enabled 的供应商仍应可解析");

        assert!(!provider.enabled);
    }

    #[test]
    fn default_catalog_keeps_disabled_providers_for_management_only() {
        let profiles = ModelProfileConfig::default();
        let catalog = profiles.catalog();
        let provider = catalog
            .providers
            .iter()
            .find(|provider| provider.id == "deepseek")
            .expect("管理目录应保留关闭供应商");

        assert!(!provider.enabled);
        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.provider_id == "deepseek")
        );
        assert!(profiles.model("deepseek", "deepseek-v4-pro").is_none());
    }

    #[test]
    fn provider_enable_requires_key_and_disabling_active_provider_clears_selection() {
        let mut profiles = ModelProfileConfig::default();
        let error = profiles
            .set_provider_enabled("deepseek", true)
            .expect_err("缺少 API Key 时必须拒绝开启");
        assert!(error.to_string().contains("provider_api_key_required"));

        enable_provider(&mut profiles, "deepseek", "profile-secret");
        profiles
            .set_active_chat("deepseek", "deepseek-v4-pro")
            .expect("应选择活动模型");
        let disabled = profiles
            .set_provider_enabled("deepseek", false)
            .expect("当前活动供应商应能关闭");
        assert!(!disabled.enabled);
        assert!(profiles.active_models.chat.provider.is_empty());
        assert!(profiles.resolve_runtime().chat.provider.is_empty());
    }

    #[test]
    fn complete_provider_profile_resolves_active_runtime_parameters() {
        let mut profiles = ModelProfileConfig::default();
        enable_provider(&mut profiles, "deepseek", "profile-secret");
        profiles
            .set_active_chat("deepseek", "deepseek-v4-pro")
            .expect("应选择活动模型");

        let runtime = profiles.resolve_runtime();
        assert_eq!(runtime.chat.provider, "deepseek");
        assert_eq!(runtime.chat.api_base, "https://api.deepseek.com");
        assert_eq!(runtime.chat.api_key.as_deref(), Some("profile-secret"));
        assert_eq!(runtime.chat.model, "deepseek-v4-pro");
        assert_eq!(runtime.chat.max_tokens, 384_000);
        assert_eq!(runtime.chat.temperature, 0.7);
    }

    #[test]
    fn resolves_persona_chat_reference_without_mutating_global_selection() {
        let mut profiles = ModelProfileConfig::default();
        enable_provider(&mut profiles, "deepseek", "profile-secret");
        profiles
            .set_active_chat("deepseek", "deepseek-v4-pro")
            .expect("应选择全局活动模型");

        let resolved = profiles
            .resolve_chat_model_reference("deepseek", "deepseek-v4-flash")
            .expect("角色引用应可解析");

        assert_eq!(resolved.config.model, "deepseek-v4-flash");
        assert!(
            resolved
                .catalog
                .capabilities
                .iter()
                .any(|item| item == "chat")
        );
        assert_eq!(
            profiles.resolve_runtime().chat.model,
            "deepseek-v4-pro",
            "解析角色偏好不得改写全局活动模型"
        );
    }

    #[test]
    fn reports_stable_diagnostic_for_missing_persona_model() {
        let mut profiles = ModelProfileConfig::default();
        enable_provider(&mut profiles, "deepseek", "profile-secret");

        let error = profiles
            .resolve_chat_model_reference("deepseek", "deleted-model")
            .expect_err("失效引用必须返回可审计诊断");

        assert_eq!(error.code, "persona_model_missing");
        assert!(error.message.contains("deleted-model"));
        assert!(!error.to_string().contains("profile-secret"));
    }

    #[test]
    fn rejects_provider_alias_that_runtime_factory_cannot_create() {
        let mut profiles = ModelProfileConfig::default();
        let mut alias = profiles.providers["deepseek"].clone();
        alias.api_key = Some("profile-secret".to_string());
        alias.enabled = true;
        profiles
            .providers
            .insert("deepseek-alias".to_string(), alias);

        let catalog = profiles.catalog();
        let alias_provider = catalog
            .providers
            .iter()
            .find(|provider| provider.id == "deepseek-alias")
            .expect("目录应保留自定义 Provider 引用");
        assert_eq!(alias_provider.status, "legacy_unsupported");

        let error = profiles
            .resolve_chat_model_reference("deepseek-alias", "deepseek-v4-flash")
            .expect_err("运行时工厂不支持的 Provider ID 必须在请求前拒绝");
        assert_eq!(error.code, "persona_model_provider_unsupported");
        assert!(!error.to_string().contains("profile-secret"));
    }

    #[test]
    fn one_provider_cannot_resolve_conflicting_keys_across_model_purposes() {
        let mut profiles = ModelProfileConfig::default();
        let mut runtime = profiles.resolve_runtime();
        runtime.chat.provider = "shared-provider".to_string();
        runtime.chat.api_base = "https://api.example/v1".to_string();
        runtime.chat.api_key = Some("chat-secret".to_string());
        runtime.chat.model = "chat-model".to_string();
        runtime.audio_understanding.provider = "shared-provider".to_string();
        runtime.audio_understanding.api_base = "https://api.example/v1".to_string();
        runtime.audio_understanding.api_key = Some("audio-secret".to_string());
        runtime.audio_understanding.model = "audio-model".to_string();

        let error = profiles
            .apply_runtime_config(&runtime)
            .expect_err("同一 Provider 只能有一个 Key");
        let message = error.to_string();
        assert!(message.contains("不同模型用途"));
        assert!(!message.contains("chat-secret"));
        assert!(!message.contains("audio-secret"));
    }

    #[test]
    fn provider_debug_output_redacts_plaintext_key() {
        let mut profiles = ModelProfileConfig::default();
        profiles
            .set_provider_api_key("deepseek", Some("debug-secret".to_string()))
            .expect("应保存 Provider Key");
        let output = format!("{:?}", profiles.providers["deepseek"]);
        assert!(output.contains("api_key_configured"));
        assert!(!output.contains("debug-secret"));
    }
}
