//! 运行时模型配置模块，定义聊天、语音合成、语音识别、音频理解和 MCP 服务配置。

pub mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use store::{ModelConfigStore, ModelConfigStoreError};

/// 模型密钥所属的运行配置段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSecretSlot {
    Chat,
    Tts,
    Asr,
    AudioUnderstanding,
}

impl ModelSecretSlot {
    /// 返回该配置段允许使用的系统凭据 account 前缀。
    pub fn account_prefix(self) -> &'static str {
        match self {
            Self::Chat => "model.chat.v2.",
            Self::Tts => "model.tts.v2.",
            Self::Asr => "model.asr.v2.",
            Self::AudioUnderstanding => "model.audio-understanding.v2.",
        }
    }

    /// 返回升级前使用的固定 account，仅用于一次性复制迁移。
    pub fn legacy_account(self) -> &'static str {
        match self {
            Self::Chat => "model.chat",
            Self::Tts => "model.tts",
            Self::Asr => "model.asr",
            Self::AudioUnderstanding => "model.audio-understanding",
        }
    }
}

/// 配置文件中的非敏感凭据引用。
///
/// `account` 每次替换密钥时都会生成新值。配置仅在对应密钥已经完整写入系统
/// 凭据库后原子切换到该引用，因此崩溃只能留下整组旧引用或整组新引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSecretBinding {
    pub account: String,
    pub identity: String,
}

/// 三类模型密钥当前引用及等待幂等清理的旧 account。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSecretBindings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ModelSecretBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts: Option<ModelSecretBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr: Option<ModelSecretBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_understanding: Option<ModelSecretBinding>,
    /// 已分配但尚未由配置正式引用的 account，用于启动时清理切换前崩溃现场。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_accounts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_accounts: Vec<String>,
}

impl ModelSecretBindings {
    pub fn is_empty(&self) -> bool {
        self.chat.is_none()
            && self.tts.is_none()
            && self.asr.is_none()
            && self.audio_understanding.is_none()
            && self.pending_accounts.is_empty()
            && self.retired_accounts.is_empty()
    }

    pub fn get(&self, slot: ModelSecretSlot) -> Option<&ModelSecretBinding> {
        match slot {
            ModelSecretSlot::Chat => self.chat.as_ref(),
            ModelSecretSlot::Tts => self.tts.as_ref(),
            ModelSecretSlot::Asr => self.asr.as_ref(),
            ModelSecretSlot::AudioUnderstanding => self.audio_understanding.as_ref(),
        }
    }

    pub fn set(&mut self, slot: ModelSecretSlot, binding: Option<ModelSecretBinding>) {
        match slot {
            ModelSecretSlot::Chat => self.chat = binding,
            ModelSecretSlot::Tts => self.tts = binding,
            ModelSecretSlot::Asr => self.asr = binding,
            ModelSecretSlot::AudioUnderstanding => self.audio_understanding = binding,
        }
    }
}

const TTS_PROVIDER_OPENAI_SPEECH: &str = "openai_audio_speech";

/// 聊天主模型配置。
///
/// 承载对话生成与情绪输出能力。提供器留空表示尚未配置；
/// 此时聊天不可用，前端设置页应引导用户填写。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// 聊天提供器，如 "openai"/"anthropic"；留空表示未配置。
    #[serde(default)]
    pub provider: String,
    /// 接口基础地址（如 https://api.openai.com/v1）。
    #[serde(default)]
    pub api_base: String,
    /// 接口密钥；新运行时由 `config.toml` Provider Profile 解析，旧 JSON 仅供迁移。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 旧配置兼容字段；当前运行时固定使用 `chat_completions`。
    #[serde(default = "default_chat_api_protocol")]
    pub api_protocol: String,
    /// 模型名（如 "deepseek-chat"、"gpt-4o"）。
    #[serde(default)]
    pub model: String,
    /// 单次响应最大令牌数。
    #[serde(default = "default_chat_max_tokens")]
    pub max_tokens: u32,
    /// 采样温度（0.0 - 2.0）。
    #[serde(default = "default_chat_temperature")]
    pub temperature: f64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            api_base: String::new(),
            api_key: None,
            api_protocol: default_chat_api_protocol(),
            model: String::new(),
            max_tokens: default_chat_max_tokens(),
            temperature: default_chat_temperature(),
        }
    }
}

impl LlmConfig {
    /// 是否已配置聊天能力（提供器非空即视为可用）。
    pub fn enabled(&self) -> bool {
        !self.provider.trim().is_empty()
    }
}

fn default_chat_api_protocol() -> String {
    "chat_completions".to_string()
}

fn default_chat_max_tokens() -> u32 {
    2048
}

fn default_chat_temperature() -> f64 {
    0.7
}

/// OpenAI-compatible 语音合成配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_tts_provider")]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_tts_model")]
    pub model: String,
    #[serde(default = "default_tts_voice_id")]
    pub voice_id: String,
    #[serde(default = "default_voice_speed")]
    pub speed: f32,
    #[serde(default = "default_tts_response_format")]
    pub response_format: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_tts_provider(),
            api_base: String::new(),
            api_key: None,
            model: default_tts_model(),
            voice_id: default_tts_voice_id(),
            speed: default_voice_speed(),
            response_format: default_tts_response_format(),
        }
    }
}

impl TtsConfig {
    pub fn enabled(&self) -> bool {
        self.enabled && self.has_external_required_fields()
    }

    pub fn provider_id(&self) -> String {
        match self.provider.trim().to_ascii_lowercase().as_str() {
            TTS_PROVIDER_OPENAI_SPEECH | "openai" | "openai_speech" | "openai-compatible" => {
                TTS_PROVIDER_OPENAI_SPEECH.to_string()
            }
            other => other.to_string(),
        }
    }

    pub fn is_external_provider(&self) -> bool {
        self.provider_id() == TTS_PROVIDER_OPENAI_SPEECH
    }

    pub fn has_external_required_fields(&self) -> bool {
        self.is_external_provider()
            && !self.api_base.trim().is_empty()
            && !self.model.trim().is_empty()
            && !self.voice_id.trim().is_empty()
            && !self.response_format.trim().is_empty()
    }

    pub fn runtime_engine(&self) -> String {
        self.provider_id()
    }

    pub fn ensure_default_profiles(&mut self) -> bool {
        let normalized_provider = self.provider_id();
        if self.provider == normalized_provider {
            return false;
        }
        self.provider = normalized_provider;
        true
    }
}

fn default_tts_provider() -> String {
    TTS_PROVIDER_OPENAI_SPEECH.to_string()
}

fn default_tts_model() -> String {
    "tts-1".to_string()
}

fn default_tts_voice_id() -> String {
    "alloy".to_string()
}

fn default_voice_speed() -> f32 {
    1.0
}

fn default_tts_response_format() -> String {
    "mp3".to_string()
}

/// OpenAI-compatible 语音识别配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechRecognitionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_asr_provider")]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_speech_language")]
    pub language: String,
    #[serde(default = "default_asr_response_format")]
    pub response_format: String,
}

impl Default for SpeechRecognitionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_asr_provider(),
            api_base: String::new(),
            api_key: None,
            model: String::new(),
            language: default_speech_language(),
            response_format: default_asr_response_format(),
        }
    }
}

impl SpeechRecognitionConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
            && self.provider_id() == "openai_audio_transcriptions"
            && !self.api_base.trim().is_empty()
            && !self.model.trim().is_empty()
    }

    pub fn provider_id(&self) -> String {
        match self.provider.trim().to_ascii_lowercase().as_str() {
            "openai_audio_transcriptions" | "openai" | "openai_asr" | "openai-compatible" => {
                "openai_audio_transcriptions".to_string()
            }
            other => other.to_string(),
        }
    }

    pub fn ensure_openai_compatible(&mut self) -> bool {
        let mut changed = false;
        let provider = self.provider_id();
        if self.provider != provider {
            self.provider = provider;
            changed = true;
        }
        if self.response_format != "json" {
            self.response_format = "json".to_string();
            changed = true;
        }
        changed
    }
}

fn default_asr_provider() -> String {
    "openai_audio_transcriptions".to_string()
}

fn default_speech_language() -> String {
    "zh".to_string()
}

fn default_asr_response_format() -> String {
    "json".to_string()
}

/// 语音输入策略。
///
/// 当前发布版本只实现 OpenAI-compatible 语音转文本路线。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceInputConfig {
    /// 当前策略；发布接口只接受 `speech_text`。
    #[serde(default = "default_voice_input_mode")]
    pub mode: String,
}

impl Default for VoiceInputConfig {
    fn default() -> Self {
        Self {
            mode: default_voice_input_mode(),
        }
    }
}

impl VoiceInputConfig {
    /// 将旧版曾公开但未接通的策略迁回真实可用路线。
    pub fn normalize_supported_mode(&mut self) -> bool {
        if self.mode == "speech_text" {
            return false;
        }
        self.mode = default_voice_input_mode();
        true
    }
}

fn default_voice_input_mode() -> String {
    "speech_text".to_string()
}

/// 运行时生效配置的聚合容器，供 Store 持有与持久化。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// 聊天主模型配置。
    #[serde(default)]
    pub chat: LlmConfig,
    /// 语音合成配置。
    #[serde(default)]
    pub tts: TtsConfig,
    /// 语音识别服务配置。
    #[serde(default)]
    pub speech_recognition: SpeechRecognitionConfig,
    /// 音频理解模型配置，当前仅用于设置页选择和后续多模态路线预留。
    #[serde(default)]
    pub audio_understanding: LlmConfig,
    /// 语音输入策略配置。
    #[serde(default)]
    pub voice_input: VoiceInputConfig,
    /// 旧版外部 MCP 服务配置，仅作为首次迁移到 `config.toml` 的来源。
    /// 新运行时和管理 API 不再更新该字段，保证兼容周期内原值不变。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, Value>,
    /// 系统凭据库中的非敏感、带配置身份约束的引用。
    #[serde(default, skip_serializing_if = "ModelSecretBindings::is_empty")]
    pub secret_bindings: ModelSecretBindings,
}
