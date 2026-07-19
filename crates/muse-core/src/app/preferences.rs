//! 用户级 `config.toml` 强类型配置存储。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::app::storage::{atomic_write_sensitive_synced, restrict_sensitive_file_permissions};
use crate::domain::mcp::{McpProfileConfig, McpRuntimeSnapshot};
use crate::domain::skill::SkillPreferences;
use crate::model::catalog::{
    ModelCatalog, ModelCatalogError, ModelCatalogItem, ModelCatalogModelDraft, ModelProviderCatalog,
};
use crate::model::config::{
    LlmConfig, ModelsConfig, SpeechRecognitionConfig, TtsConfig, VoiceInputConfig,
};
use crate::model::profile_config::ModelProfileConfig;

const CONFIG_FILE: &str = "config.toml";
const CONFIG_SCHEMA_VERSION: i64 = 1;

/// 配置字段诊断；固定字段路径和中文消息不得包含秘密。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigDiagnostic {
    pub code: String,
    pub field_path: String,
    pub message: String,
}

/// 动效强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionLevel {
    Full,
    Reduced,
    None,
}

impl MotionLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
            Self::None => "none",
        }
    }
}

/// 外观偏好。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppearancePreferences {
    pub theme: String,
    pub language: String,
    pub background_blur: u8,
    pub background_opacity: f64,
    pub motion_level: MotionLevel,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            language: "zh-CN".to_string(),
            background_blur: 18,
            background_opacity: 1.0,
            motion_level: MotionLevel::Full,
        }
    }
}

/// 对话输入与恢复偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationPreferences {
    pub send_key: String,
    pub restore_last_session: bool,
}

impl Default for ConversationPreferences {
    fn default() -> Self {
        Self {
            send_key: "enter".to_string(),
            restore_last_session: true,
        }
    }
}

/// 非敏感语音行为偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePreferences {
    pub auto_play: bool,
    pub input_language: String,
}

impl Default for VoicePreferences {
    fn default() -> Self {
        Self {
            auto_play: false,
            input_language: "zh".to_string(),
        }
    }
}

/// 更新检查偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePreferences {
    pub check_on_startup: bool,
}

impl Default for UpdatePreferences {
    fn default() -> Self {
        Self {
            check_on_startup: true,
        }
    }
}

/// 当前已实现的用户级声明式配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MuseConfig {
    pub schema_version: u32,
    pub appearance: AppearancePreferences,
    pub conversation: ConversationPreferences,
    pub voice: VoicePreferences,
    pub updates: UpdatePreferences,
    /// Provider Profile 与活动模型选择。API 响应不得直接序列化这个字段。
    #[serde(skip_serializing)]
    pub model_profiles: ModelProfileConfig,
    /// MCP Server Profile。API 响应不得直接序列化其中的明文 API Key。
    #[serde(skip_serializing)]
    pub mcp_profiles: McpProfileConfig,
    /// Agent Skills 启停覆盖；Skill 正文仍由目录中的 `SKILL.md` 承载。
    #[serde(skip_serializing)]
    pub skill_preferences: SkillPreferences,
}

impl Default for MuseConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION as u32,
            appearance: AppearancePreferences::default(),
            conversation: ConversationPreferences::default(),
            voice: VoicePreferences::default(),
            updates: UpdatePreferences::default(),
            model_profiles: ModelProfileConfig::default(),
            mcp_profiles: McpProfileConfig::default(),
            skill_preferences: SkillPreferences::default(),
        }
    }
}

/// 本地 API 返回的配置快照。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MuseConfigSnapshot {
    pub config: MuseConfig,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

/// `config.toml` 加载或持久化错误。
#[derive(Debug)]
pub enum MuseConfigStoreError {
    Io(std::io::Error),
    Parse(toml_edit::TomlError),
    Serialize(toml_edit::ser::Error),
    UnsupportedVersion(i64),
    Validation(ConfigDiagnostic),
    Conflict(ConfigDiagnostic),
}

impl std::fmt::Display for MuseConfigStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "用户配置文件读写失败：{error}"),
            Self::Parse(error) => write!(formatter, "用户配置文件 TOML 解析失败：{error}"),
            Self::Serialize(error) => write!(formatter, "用户配置文件 TOML 序列化失败：{error}"),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "用户配置 schema_version={version} 高于当前支持的 {CONFIG_SCHEMA_VERSION}"
            ),
            Self::Validation(diagnostic) => write!(formatter, "{}", diagnostic.message),
            Self::Conflict(diagnostic) => write!(formatter, "{}", diagnostic.message),
        }
    }
}

impl std::error::Error for MuseConfigStoreError {}

impl From<std::io::Error> for MuseConfigStoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<toml_edit::TomlError> for MuseConfigStoreError {
    fn from(value: toml_edit::TomlError) -> Self {
        Self::Parse(value)
    }
}

impl From<toml_edit::ser::Error> for MuseConfigStoreError {
    fn from(value: toml_edit::ser::Error) -> Self {
        Self::Serialize(value)
    }
}

/// 保留 TOML 注释、顺序和未知字段的统一用户配置存储。
///
/// 外观偏好与完整 Provider Profile 通过这个单一写入器发布，不能为同一文件建立平行 Store。
#[derive(Debug, Clone)]
pub struct MuseConfigStore {
    storage_path: PathBuf,
    document: DocumentMut,
    config: MuseConfig,
    diagnostics: Vec<ConfigDiagnostic>,
    content_digest: [u8; 32],
    runtime_models: ModelsConfig,
}

impl MuseConfigStore {
    /// 加载用户配置；缺失时创建带完整默认值的新文件。
    pub fn load_from_dir(base_dir: impl AsRef<Path>) -> Result<Self, MuseConfigStoreError> {
        let storage_path = base_dir.as_ref().join(CONFIG_FILE);
        reject_non_regular_config_path(&storage_path)?;
        if !storage_path.exists() {
            let document = default_document();
            let content = document.to_string();
            atomic_write_sensitive_synced(&storage_path, content.as_bytes())?;
            return Ok(Self {
                storage_path,
                document,
                config: MuseConfig::default(),
                diagnostics: Vec::new(),
                content_digest: content_digest(content.as_bytes()),
                runtime_models: MuseConfig::default().model_profiles.resolve_runtime(),
            });
        }

        restrict_sensitive_file_permissions(&storage_path)?;
        let content = std::fs::read_to_string(&storage_path)?;
        let document = content.parse::<DocumentMut>()?;
        let (config, diagnostics) = decode_document(&document)?;
        let content_digest = content_digest(content.as_bytes());
        let runtime_models = config.model_profiles.resolve_runtime();
        Ok(Self {
            storage_path,
            document,
            config,
            diagnostics,
            content_digest,
            runtime_models,
        })
    }

    /// 返回强类型配置与字段诊断。
    pub fn snapshot(&self) -> MuseConfigSnapshot {
        MuseConfigSnapshot {
            config: self.config.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    /// 更新外观段；成功发布前不改变内存事实，失败时保留原文件和快照。
    pub fn update_appearance(
        &mut self,
        appearance: AppearancePreferences,
    ) -> Result<MuseConfigSnapshot, MuseConfigStoreError> {
        self.reject_external_modification()?;
        validate_appearance(&appearance)?;
        let mut next_document = self.document.clone();
        write_appearance(&mut next_document, &appearance)?;
        set_value_preserving_decor(
            next_document.as_table_mut(),
            "schema_version",
            Value::from(CONFIG_SCHEMA_VERSION),
        );
        let (config, diagnostics) = decode_document(&next_document)?;
        let content = next_document.to_string();
        atomic_write_sensitive_synced(&self.storage_path, content.as_bytes())?;
        self.document = next_document;
        self.config = config;
        self.diagnostics = diagnostics;
        self.content_digest = content_digest(content.as_bytes());
        self.runtime_models = self.config.model_profiles.resolve_runtime();
        Ok(self.snapshot())
    }

    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Provider Profile 是否已经真实发布到 TOML；内存默认值不能冒充迁移完成。
    pub fn has_published_model_profiles(&self) -> bool {
        self.document
            .get("providers")
            .is_some_and(Item::is_table_like)
            && self
                .document
                .get("active_models")
                .is_some_and(Item::is_table_like)
    }

    pub fn model_profiles(&self) -> &ModelProfileConfig {
        &self.config.model_profiles
    }

    /// MCP Server Profile 是否已经真实发布到 TOML。
    pub fn has_published_mcp_profiles(&self) -> bool {
        self.document
            .get("mcp_servers")
            .is_some_and(Item::is_table_like)
    }

    pub fn mcp_profiles(&self) -> &McpProfileConfig {
        &self.config.mcp_profiles
    }

    pub fn mcp_profiles_are_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "config_mcp_profiles_invalid")
    }

    pub fn mcp_runtime_snapshot(&self) -> McpRuntimeSnapshot {
        self.config
            .mcp_profiles
            .runtime_snapshot(self.storage_path.clone())
    }

    pub fn skill_preferences(&self) -> &SkillPreferences {
        &self.config.skill_preferences
    }

    /// Skill 管理与迁移共用的原子启停配置发布入口。
    pub fn update_skill_preferences(
        &mut self,
        preferences: SkillPreferences,
    ) -> Result<(), MuseConfigStoreError> {
        self.reject_external_modification()?;
        preferences.validate().map_err(|message| {
            validation(
                "config_skill_preferences_invalid",
                "skills.config",
                &message,
            )
        })?;
        self.commit_skill_preferences(preferences)
    }

    /// MCP 管理 API 和迁移器共用的原子发布入口。
    pub fn update_mcp_profiles(
        &mut self,
        profiles: McpProfileConfig,
    ) -> Result<(), MuseConfigStoreError> {
        self.reject_external_modification()?;
        profiles.validate().map_err(|message| {
            validation("config_mcp_profiles_invalid", "mcp_servers", &message)
        })?;
        self.commit_mcp_profiles(profiles)
    }

    pub fn publish_migrated_mcp_profiles(
        &mut self,
        profiles: McpProfileConfig,
    ) -> Result<(), MuseConfigStoreError> {
        self.update_mcp_profiles(profiles)
    }

    /// Provider Profile 是否通过强类型解析；迁移器据此阻止损坏配置触发旧凭据清理。
    pub fn model_profiles_are_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "config_model_profiles_invalid")
    }

    /// 重新读取手工修改后的配置；返回是否观察到外部 revision 变化。
    pub fn refresh_from_disk(&mut self) -> Result<bool, MuseConfigStoreError> {
        let content = std::fs::read_to_string(&self.storage_path)?;
        let current_digest = content_digest(content.as_bytes());
        if current_digest == self.content_digest {
            return Ok(false);
        }

        let document = content.parse::<DocumentMut>()?;
        let (config, diagnostics) = decode_document(&document)?;
        self.document = document;
        self.config = config;
        self.diagnostics = diagnostics;
        self.content_digest = current_digest;
        self.runtime_models = self.config.model_profiles.resolve_runtime();
        Ok(true)
    }

    /// 当前运行时兼容配置；只由完整 Provider Profile 解析生成。
    pub fn config(&self) -> &ModelsConfig {
        &self.runtime_models
    }

    pub fn chat(&self) -> &LlmConfig {
        &self.runtime_models.chat
    }

    pub fn tts(&self) -> &TtsConfig {
        &self.runtime_models.tts
    }

    pub fn speech_recognition(&self) -> &SpeechRecognitionConfig {
        &self.runtime_models.speech_recognition
    }

    pub fn audio_understanding(&self) -> &LlmConfig {
        &self.runtime_models.audio_understanding
    }

    pub fn voice_input(&self) -> &VoiceInputConfig {
        &self.runtime_models.voice_input
    }

    pub fn model_catalog(&self) -> ModelCatalog {
        self.config.model_profiles.catalog()
    }

    pub fn model_provider(&self, provider_id: &str) -> Option<ModelProviderCatalog> {
        self.config.model_profiles.provider(provider_id)
    }

    pub fn managed_model_provider(&self, provider_id: &str) -> Option<ModelProviderCatalog> {
        self.config.model_profiles.managed_provider(provider_id)
    }

    pub fn catalog_model(&self, provider_id: &str, model_id: &str) -> Option<ModelCatalogItem> {
        self.config.model_profiles.model(provider_id, model_id)
    }

    /// 兼容现有设置 API，把四段模型配置合并回完整 Provider Profile 后原子发布。
    pub fn update_models_config(
        &mut self,
        models: ModelsConfig,
    ) -> Result<ModelsConfig, MuseConfigStoreError> {
        self.reject_external_modification()?;
        let mut next = self.config.model_profiles.clone();
        next.apply_runtime_config(&models).map_err(|error| {
            validation("model_profile_invalid", "providers", &error.to_string())
        })?;
        self.commit_model_profiles(next)?;
        Ok(self.runtime_models.clone())
    }

    pub fn update_provider_api_key(
        &mut self,
        provider_id: &str,
        api_key: Option<String>,
    ) -> Result<bool, ModelCatalogError> {
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        let configured = next.set_provider_api_key(provider_id, api_key)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)?;
        Ok(configured)
    }

    pub fn update_provider_enabled(
        &mut self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<ModelProviderCatalog, ModelCatalogError> {
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        let provider = next.set_provider_enabled(provider_id, enabled)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)?;
        Ok(provider)
    }

    pub fn create_catalog_model(
        &mut self,
        draft: ModelCatalogModelDraft,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        let item = next.create_model(draft)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)?;
        Ok(item)
    }

    pub fn update_catalog_model(
        &mut self,
        draft: ModelCatalogModelDraft,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        let item = next.update_model(draft)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)?;
        Ok(item)
    }

    pub fn disable_catalog_model(
        &mut self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), ModelCatalogError> {
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        next.disable_model(provider_id, model_id)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)
    }

    pub fn ensure_runtime_model(
        &mut self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        if let Some(model) = self.catalog_model(provider_id, model_id) {
            return Ok(model);
        }
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        let item = next.ensure_runtime_model(provider_id, model_id)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)?;
        Ok(item)
    }

    pub fn update_active_chat_model(
        &mut self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelsConfig, ModelCatalogError> {
        self.reject_external_modification()
            .map_err(ModelCatalogError::Config)?;
        let mut next = self.config.model_profiles.clone();
        next.set_active_chat(provider_id, model_id)?;
        self.commit_model_profiles(next)
            .map_err(ModelCatalogError::Config)?;
        Ok(self.runtime_models.clone())
    }

    /// 首次模型迁移使用；调用方完成旧源读取和凭据回读后一次发布完整配置。
    pub fn publish_migrated_model_profiles(
        &mut self,
        profiles: ModelProfileConfig,
    ) -> Result<(), MuseConfigStoreError> {
        self.reject_external_modification()?;
        self.commit_model_profiles(profiles)
    }

    fn commit_model_profiles(
        &mut self,
        profiles: ModelProfileConfig,
    ) -> Result<(), MuseConfigStoreError> {
        let mut next_document = self.document.clone();
        write_model_profiles(&mut next_document, &profiles)?;
        let (config, diagnostics) = decode_document(&next_document)?;
        let content = next_document.to_string();
        atomic_write_sensitive_synced(&self.storage_path, content.as_bytes())?;
        self.document = next_document;
        self.runtime_models = config.model_profiles.resolve_runtime();
        self.config = config;
        self.diagnostics = diagnostics;
        self.content_digest = content_digest(content.as_bytes());
        Ok(())
    }

    fn commit_mcp_profiles(
        &mut self,
        profiles: McpProfileConfig,
    ) -> Result<(), MuseConfigStoreError> {
        let mut next_document = self.document.clone();
        write_mcp_profiles(&mut next_document, &profiles)?;
        let (config, diagnostics) = decode_document(&next_document)?;
        let content = next_document.to_string();
        atomic_write_sensitive_synced(&self.storage_path, content.as_bytes())?;
        self.document = next_document;
        self.runtime_models = config.model_profiles.resolve_runtime();
        self.config = config;
        self.diagnostics = diagnostics;
        self.content_digest = content_digest(content.as_bytes());
        Ok(())
    }

    fn commit_skill_preferences(
        &mut self,
        preferences: SkillPreferences,
    ) -> Result<(), MuseConfigStoreError> {
        let mut next_document = self.document.clone();
        write_skill_preferences(&mut next_document, &preferences)?;
        let (config, diagnostics) = decode_document(&next_document)?;
        let content = next_document.to_string();
        atomic_write_sensitive_synced(&self.storage_path, content.as_bytes())?;
        self.document = next_document;
        self.runtime_models = config.model_profiles.resolve_runtime();
        self.config = config;
        self.diagnostics = diagnostics;
        self.content_digest = content_digest(content.as_bytes());
        Ok(())
    }

    /// 跨 `SKILL.md` 与 `config.toml` 事务失败时恢复已发布前的配置快照。
    pub(crate) fn restore_snapshot(&mut self, previous: &Self) -> Result<(), MuseConfigStoreError> {
        let current = std::fs::read(&self.storage_path)?;
        if content_digest(&current) != self.content_digest {
            return Err(MuseConfigStoreError::Conflict(diagnostic(
                "config_revision_conflict",
                "config.toml",
                "配置文件在回滚前已被外部修改，已停止自动恢复。",
            )));
        }
        let content = previous.document.to_string();
        atomic_write_sensitive_synced(&self.storage_path, content.as_bytes())?;
        *self = previous.clone();
        Ok(())
    }

    fn reject_external_modification(&mut self) -> Result<(), MuseConfigStoreError> {
        if !self.refresh_from_disk()? {
            return Ok(());
        }
        Err(MuseConfigStoreError::Conflict(diagnostic(
            "config_revision_conflict",
            "config.toml",
            "配置文件已在应用外部修改，本次保存已取消，请重新加载后再试。",
        )))
    }
}

fn content_digest(content: &[u8]) -> [u8; 32] {
    Sha256::digest(content).into()
}

fn validate_appearance(appearance: &AppearancePreferences) -> Result<(), MuseConfigStoreError> {
    if !matches!(appearance.theme.as_str(), "system" | "light" | "dark") {
        return Err(validation(
            "config_value_invalid",
            "appearance.theme",
            "外观主题必须是 system、light 或 dark。",
        ));
    }
    if appearance.language.trim().is_empty() || appearance.language.len() > 32 {
        return Err(validation(
            "config_value_invalid",
            "appearance.language",
            "界面语言必须是 1 到 32 个字符。",
        ));
    }
    if appearance.background_blur > 30 {
        return Err(validation(
            "config_value_out_of_range",
            "appearance.background_blur",
            "背景模糊必须在 0 到 30 之间。",
        ));
    }
    if !appearance.background_opacity.is_finite()
        || !(0.2..=1.0).contains(&appearance.background_opacity)
    {
        return Err(validation(
            "config_value_out_of_range",
            "appearance.background_opacity",
            "背景可见度必须在 0.2 到 1.0 之间。",
        ));
    }
    Ok(())
}

fn validation(code: &str, field_path: &str, message: &str) -> MuseConfigStoreError {
    MuseConfigStoreError::Validation(ConfigDiagnostic {
        code: code.to_string(),
        field_path: field_path.to_string(),
        message: message.to_string(),
    })
}

fn default_document() -> DocumentMut {
    r#"schema_version = 1

[appearance]
theme = "system"
language = "zh-CN"
background_blur = 18
background_opacity = 1.0
motion_level = "full"

[conversation]
send_key = "enter"
restore_last_session = true

[voice]
auto_play = false
input_language = "zh"

[updates]
check_on_startup = true
"#
    .parse()
    .expect("内置 config.toml 模板必须有效")
}

fn decode_document(
    document: &DocumentMut,
) -> Result<(MuseConfig, Vec<ConfigDiagnostic>), MuseConfigStoreError> {
    let mut config = MuseConfig::default();
    let mut diagnostics = Vec::new();
    collect_unknown_fields(document, &mut diagnostics);

    match document.get("schema_version").and_then(Item::as_integer) {
        Some(version) if version > CONFIG_SCHEMA_VERSION => {
            return Err(MuseConfigStoreError::UnsupportedVersion(version));
        }
        Some(version) if version == CONFIG_SCHEMA_VERSION => {
            config.schema_version = version as u32;
        }
        Some(_) => diagnostics.push(diagnostic(
            "config_version_unsupported",
            "schema_version",
            "schema_version 必须为 1。",
        )),
        None => diagnostics.push(diagnostic(
            "config_type_invalid",
            "schema_version",
            "schema_version 必须是整数 1。",
        )),
    }

    if let Some(table) = read_table(document, "appearance", &mut diagnostics) {
        config.appearance.theme = read_enum_string(
            table,
            "theme",
            "appearance.theme",
            &config.appearance.theme,
            &["system", "light", "dark"],
            &mut diagnostics,
        );
        config.appearance.language = read_non_empty_string(
            table,
            "language",
            "appearance.language",
            &config.appearance.language,
            32,
            &mut diagnostics,
        );
        config.appearance.background_blur = read_integer_range(
            table,
            "background_blur",
            "appearance.background_blur",
            config.appearance.background_blur as i64,
            0,
            30,
            &mut diagnostics,
        ) as u8;
        config.appearance.background_opacity = read_float_range(
            table,
            "background_opacity",
            "appearance.background_opacity",
            config.appearance.background_opacity,
            0.2,
            1.0,
            &mut diagnostics,
        );
        let motion = read_enum_string(
            table,
            "motion_level",
            "appearance.motion_level",
            config.appearance.motion_level.as_str(),
            &["full", "reduced", "none"],
            &mut diagnostics,
        );
        config.appearance.motion_level = match motion.as_str() {
            "reduced" => MotionLevel::Reduced,
            "none" => MotionLevel::None,
            _ => MotionLevel::Full,
        };
    }

    if let Some(table) = read_table(document, "conversation", &mut diagnostics) {
        config.conversation.send_key = read_enum_string(
            table,
            "send_key",
            "conversation.send_key",
            &config.conversation.send_key,
            &["enter", "mod_enter"],
            &mut diagnostics,
        );
        config.conversation.restore_last_session = read_bool(
            table,
            "restore_last_session",
            "conversation.restore_last_session",
            config.conversation.restore_last_session,
            &mut diagnostics,
        );
    }

    if let Some(table) = read_table(document, "voice", &mut diagnostics) {
        config.voice.auto_play = read_bool(
            table,
            "auto_play",
            "voice.auto_play",
            config.voice.auto_play,
            &mut diagnostics,
        );
        config.voice.input_language = read_non_empty_string(
            table,
            "input_language",
            "voice.input_language",
            &config.voice.input_language,
            32,
            &mut diagnostics,
        );
    }

    if let Some(table) = read_table(document, "updates", &mut diagnostics) {
        config.updates.check_on_startup = read_bool(
            table,
            "check_on_startup",
            "updates.check_on_startup",
            config.updates.check_on_startup,
            &mut diagnostics,
        );
    }
    config.model_profiles = decode_model_profiles(document, &mut diagnostics);
    config.mcp_profiles = decode_mcp_profiles(document, &mut diagnostics);
    config.skill_preferences = decode_skill_preferences(document, &mut diagnostics);
    Ok((config, diagnostics))
}

fn collect_unknown_fields(document: &DocumentMut, diagnostics: &mut Vec<ConfigDiagnostic>) {
    const TOP_LEVEL: &[&str] = &[
        "schema_version",
        "appearance",
        "conversation",
        "voice",
        "updates",
        "providers",
        "active_models",
        "mcp_servers",
        "skills",
    ];
    for (key, _) in document.iter() {
        if !TOP_LEVEL.contains(&key) {
            diagnostics.push(diagnostic(
                "config_unknown_field",
                key,
                &format!("配置字段 `{key}` 暂不识别，保存时会原样保留。"),
            ));
        }
    }
    for (section, fields) in [
        (
            "appearance",
            &[
                "theme",
                "language",
                "background_blur",
                "background_opacity",
                "motion_level",
            ][..],
        ),
        ("conversation", &["send_key", "restore_last_session"][..]),
        ("voice", &["auto_play", "input_language"][..]),
        ("updates", &["check_on_startup"][..]),
        ("skills", &["config"][..]),
    ] {
        let Some(table) = document.get(section).and_then(Item::as_table) else {
            continue;
        };
        for (key, _) in table.iter() {
            if !fields.contains(&key) {
                let path = format!("{section}.{key}");
                diagnostics.push(diagnostic(
                    "config_unknown_field",
                    &path,
                    &format!("配置字段 `{path}` 暂不识别，保存时会原样保留。"),
                ));
            }
        }
    }
}

fn decode_model_profiles(
    document: &DocumentMut,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> ModelProfileConfig {
    if document.get("providers").is_none() && document.get("active_models").is_none() {
        return ModelProfileConfig::default();
    }
    match toml_edit::de::from_document::<ModelProfileConfig>(document.clone()) {
        Ok(config) => config,
        Err(_) => {
            diagnostics.push(diagnostic(
                "config_model_profiles_invalid",
                "providers",
                "Provider Profile 存在类型或结构错误；为避免泄漏 API Key，详情不会进入诊断。",
            ));
            ModelProfileConfig::default()
        }
    }
}

fn decode_mcp_profiles(
    document: &DocumentMut,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> McpProfileConfig {
    if document.get("mcp_servers").is_none() {
        return McpProfileConfig::default();
    }
    match toml_edit::de::from_document::<McpProfileConfig>(document.clone()) {
        Ok(config) => match config.validate() {
            Ok(()) => config,
            Err(_) => {
                diagnostics.push(diagnostic(
                    "config_mcp_profiles_invalid",
                    "mcp_servers",
                    "MCP Server Profile 存在无效字段；为避免泄漏 API Key，详情不会进入诊断。",
                ));
                McpProfileConfig::default()
            }
        },
        Err(_) => {
            diagnostics.push(diagnostic(
                "config_mcp_profiles_invalid",
                "mcp_servers",
                "MCP Server Profile 存在类型或结构错误；为避免泄漏 API Key，详情不会进入诊断。",
            ));
            McpProfileConfig::default()
        }
    }
}

#[derive(Deserialize)]
struct SkillPreferencesDocument {
    #[serde(default)]
    skills: SkillPreferences,
}

fn decode_skill_preferences(
    document: &DocumentMut,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> SkillPreferences {
    if document.get("skills").is_none() {
        return SkillPreferences::default();
    }
    match toml_edit::de::from_document::<SkillPreferencesDocument>(document.clone()) {
        Ok(config) => match config.skills.validate() {
            Ok(()) => config.skills,
            Err(_) => {
                diagnostics.push(diagnostic(
                    "config_skill_preferences_invalid",
                    "skills.config",
                    "Skill 启停配置存在无效路径或重复项，当前未应用这些覆盖。",
                ));
                SkillPreferences::default()
            }
        },
        Err(_) => {
            diagnostics.push(diagnostic(
                "config_skill_preferences_invalid",
                "skills.config",
                "Skill 启停配置存在类型或结构错误，当前未应用这些覆盖。",
            ));
            SkillPreferences::default()
        }
    }
}

fn write_model_profiles(
    document: &mut DocumentMut,
    profiles: &ModelProfileConfig,
) -> Result<(), MuseConfigStoreError> {
    let mut serialized = toml_edit::ser::to_document(profiles)?;
    for (_, item) in serialized.as_table_mut().iter_mut() {
        expand_inline_tables(item);
    }
    for key in ["providers", "active_models"] {
        let source = serialized
            .get(key)
            .cloned()
            .unwrap_or_else(|| Item::Table(Table::new()));
        match document.get_mut(key) {
            Some(target) => {
                expand_inline_tables(target);
                merge_typed_item(target, &source);
            }
            None => {
                document.insert(key, source);
            }
        }
    }

    // `None` 不会被 TOML 序列化；删除密钥动作必须显式移除旧值。
    if let Some(provider_tables) = document.get_mut("providers").and_then(Item::as_table_mut) {
        for (provider_id, provider) in &profiles.providers {
            if provider.api_key.is_none()
                && let Some(table) = provider_tables
                    .get_mut(provider_id)
                    .and_then(Item::as_table_mut)
            {
                table.remove("api_key");
            }
        }
    }
    Ok(())
}

fn write_mcp_profiles(
    document: &mut DocumentMut,
    profiles: &McpProfileConfig,
) -> Result<(), MuseConfigStoreError> {
    let mut serialized = toml_edit::ser::to_document(profiles)?;
    for (_, item) in serialized.as_table_mut().iter_mut() {
        expand_inline_tables(item);
    }
    let source = serialized
        .get("mcp_servers")
        .cloned()
        .unwrap_or_else(|| Item::Table(Table::new()));
    if document.get("mcp_servers").is_none() {
        document.insert("mcp_servers", Item::Table(Table::new()));
    }
    let target = document.get_mut("mcp_servers").expect("mcp_servers 已创建");
    expand_inline_tables(target);
    let Some(target_table) = target.as_table_mut() else {
        return Err(validation(
            "config_type_invalid",
            "mcp_servers",
            "配置段 `mcp_servers` 必须是 TOML 表；为避免覆盖未知内容，本次保存已取消。",
        ));
    };
    let source_table = source.as_table().expect("序列化的 mcp_servers 必须是表");

    target_table.retain(|server_name, _| source_table.contains_key(server_name));
    const KNOWN_FIELDS: &[&str] = &[
        "type",
        "enabled",
        "request_timeout_ms",
        "enabled_tools",
        "disabled_tools",
        "command",
        "args",
        "cwd",
        "url",
        "env",
        "secret_env",
        "headers",
        "secret_headers",
    ];
    for (server_name, source_server) in source_table.iter() {
        if !target_table.contains_key(server_name) {
            target_table.insert(server_name, source_server.clone());
            continue;
        }
        let target_server = target_table
            .get_mut(server_name)
            .expect("MCP server 已存在");
        expand_inline_tables(target_server);
        let Some(target_server_table) = target_server.as_table_mut() else {
            return Err(validation(
                "config_type_invalid",
                &format!("mcp_servers.{server_name}"),
                "MCP Server Profile 必须是 TOML 表；本次保存已取消。",
            ));
        };
        let source_server_table = source_server
            .as_table()
            .expect("序列化的 MCP server 必须是表");
        for field in KNOWN_FIELDS {
            if !source_server_table.contains_key(field) {
                target_server_table.remove(field);
            }
        }
        merge_typed_item(target_server, source_server);
    }
    Ok(())
}

fn write_skill_preferences(
    document: &mut DocumentMut,
    preferences: &SkillPreferences,
) -> Result<(), MuseConfigStoreError> {
    preferences.validate().map_err(|message| {
        validation(
            "config_skill_preferences_invalid",
            "skills.config",
            &message,
        )
    })?;
    if document.get("skills").is_none() {
        document.insert("skills", Item::Table(Table::new()));
    }
    let Some(skills) = document.get_mut("skills").and_then(Item::as_table_mut) else {
        return Err(validation(
            "config_type_invalid",
            "skills",
            "配置段 `skills` 必须是 TOML 表；本次保存已取消。",
        ));
    };
    let existing = match skills.get("config") {
        Some(item) => Some(item.as_array_of_tables().ok_or_else(|| {
            validation(
                "config_type_invalid",
                "skills.config",
                "配置段 `skills.config` 必须是 TOML 表数组；本次保存已取消。",
            )
        })?),
        None => None,
    };
    let mut entries = toml_edit::ArrayOfTables::new();
    for entry in &preferences.config {
        let mut table = existing
            .and_then(|tables| {
                tables.iter().find(|table| {
                    table.get("path").and_then(Item::as_str) == Some(entry.path.as_str())
                })
            })
            .cloned()
            .unwrap_or_default();
        set_value_preserving_decor(&mut table, "path", Value::from(entry.path.as_str()));
        set_value_preserving_decor(&mut table, "enabled", Value::from(entry.enabled));
        entries.push(table);
    }
    skills.insert("config", Item::ArrayOfTables(entries));
    Ok(())
}

/// Serde 会把嵌套结构默认编码为内联表。Provider Profile 需要供用户直接维护，
/// 因而发布前统一展开为分层表；已有内联配置也在首次保存时无损升级。
fn expand_inline_tables(item: &mut Item) {
    if item.is_inline_table() {
        let current = std::mem::take(item);
        let Item::Value(Value::InlineTable(inline)) = current else {
            unreachable!("is_inline_table 已确认类型")
        };
        *item = Item::Table(inline.into_table());
    }
    if let Some(table) = item.as_table_mut() {
        for (_, child) in table.iter_mut() {
            expand_inline_tables(child);
        }
    }
}

fn merge_typed_item(target: &mut Item, source: &Item) {
    if let (Some(target_table), Some(source_table)) = (target.as_table_mut(), source.as_table()) {
        for (key, source_item) in source_table.iter() {
            match target_table.get_mut(key) {
                Some(target_item) => merge_typed_item(target_item, source_item),
                None => {
                    target_table.insert(key, source_item.clone());
                }
            }
        }
        return;
    }
    let decor = target.as_value().map(|value| value.decor().clone());
    *target = source.clone();
    if let Some(decor) = decor
        && let Some(value) = target.as_value_mut()
    {
        *value.decor_mut() = decor;
    }
}

fn read_table<'a>(
    document: &'a DocumentMut,
    key: &str,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<&'a Table> {
    match document.get(key) {
        Some(item) => match item.as_table() {
            Some(table) => Some(table),
            None => {
                diagnostics.push(diagnostic(
                    "config_type_invalid",
                    key,
                    &format!("配置段 `{key}` 必须是 TOML 表。"),
                ));
                None
            }
        },
        None => {
            diagnostics.push(diagnostic(
                "config_field_missing",
                key,
                &format!("配置段 `{key}` 缺失，当前使用内置默认值。"),
            ));
            None
        }
    }
}

fn read_enum_string(
    table: &Table,
    key: &str,
    path: &str,
    default: &str,
    allowed: &[&str],
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> String {
    match table.get(key).and_then(Item::as_str) {
        Some(value) if allowed.contains(&value) => value.to_string(),
        Some(_) => {
            diagnostics.push(diagnostic(
                "config_value_invalid",
                path,
                &format!("配置字段 `{path}` 的值不受支持，当前使用默认值。"),
            ));
            default.to_string()
        }
        None => {
            diagnostics.push(diagnostic(
                "config_type_invalid",
                path,
                &format!("配置字段 `{path}` 必须是字符串，当前使用默认值。"),
            ));
            default.to_string()
        }
    }
}

fn read_non_empty_string(
    table: &Table,
    key: &str,
    path: &str,
    default: &str,
    maximum_length: usize,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> String {
    match table.get(key).and_then(Item::as_str) {
        Some(value) if !value.trim().is_empty() && value.len() <= maximum_length => {
            value.to_string()
        }
        Some(_) => {
            diagnostics.push(diagnostic(
                "config_value_invalid",
                path,
                &format!("配置字段 `{path}` 不是有效的非空文本，当前使用默认值。"),
            ));
            default.to_string()
        }
        None => {
            diagnostics.push(diagnostic(
                "config_type_invalid",
                path,
                &format!("配置字段 `{path}` 必须是字符串，当前使用默认值。"),
            ));
            default.to_string()
        }
    }
}

fn read_integer_range(
    table: &Table,
    key: &str,
    path: &str,
    default: i64,
    minimum: i64,
    maximum: i64,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> i64 {
    match table.get(key).and_then(Item::as_integer) {
        Some(value) if (minimum..=maximum).contains(&value) => value,
        Some(_) => {
            diagnostics.push(diagnostic(
                "config_value_out_of_range",
                path,
                &format!("配置字段 `{path}` 超出允许范围，当前使用默认值。"),
            ));
            default
        }
        None => {
            diagnostics.push(diagnostic(
                "config_type_invalid",
                path,
                &format!("配置字段 `{path}` 必须是整数，当前使用默认值。"),
            ));
            default
        }
    }
}

fn read_float_range(
    table: &Table,
    key: &str,
    path: &str,
    default: f64,
    minimum: f64,
    maximum: f64,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> f64 {
    let value = table.get(key).and_then(|item| {
        item.as_float()
            .or_else(|| item.as_integer().map(|integer| integer as f64))
    });
    match value {
        Some(value) if value.is_finite() && (minimum..=maximum).contains(&value) => value,
        Some(_) => {
            diagnostics.push(diagnostic(
                "config_value_out_of_range",
                path,
                &format!("配置字段 `{path}` 超出允许范围，当前使用默认值。"),
            ));
            default
        }
        None => {
            diagnostics.push(diagnostic(
                "config_type_invalid",
                path,
                &format!("配置字段 `{path}` 必须是数字，当前使用默认值。"),
            ));
            default
        }
    }
}

fn read_bool(
    table: &Table,
    key: &str,
    path: &str,
    default: bool,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> bool {
    match table.get(key).and_then(Item::as_bool) {
        Some(value) => value,
        None => {
            diagnostics.push(diagnostic(
                "config_type_invalid",
                path,
                &format!("配置字段 `{path}` 必须是布尔值，当前使用默认值。"),
            ));
            default
        }
    }
}

fn diagnostic(code: &str, field_path: &str, message: &str) -> ConfigDiagnostic {
    ConfigDiagnostic {
        code: code.to_string(),
        field_path: field_path.to_string(),
        message: message.to_string(),
    }
}

fn write_appearance(
    document: &mut DocumentMut,
    appearance: &AppearancePreferences,
) -> Result<(), MuseConfigStoreError> {
    if document
        .get("appearance")
        .is_some_and(|item| !item.is_table())
    {
        return Err(validation(
            "config_type_invalid",
            "appearance",
            "配置段 `appearance` 必须是 TOML 表；为避免覆盖未知内容，本次保存已取消。",
        ));
    }
    if document.get("appearance").is_none() {
        document["appearance"] = Item::Table(Table::new());
    }
    let table = document["appearance"]
        .as_table_mut()
        .expect("appearance 已转换为表");
    set_value_preserving_decor(table, "theme", Value::from(appearance.theme.as_str()));
    set_value_preserving_decor(table, "language", Value::from(appearance.language.as_str()));
    set_value_preserving_decor(
        table,
        "background_blur",
        Value::from(i64::from(appearance.background_blur)),
    );
    set_value_preserving_decor(
        table,
        "background_opacity",
        Value::from(appearance.background_opacity),
    );
    set_value_preserving_decor(
        table,
        "motion_level",
        Value::from(appearance.motion_level.as_str()),
    );
    Ok(())
}

fn set_value_preserving_decor(table: &mut Table, key: &str, new_value: Value) {
    if let Some(item) = table.get_mut(key) {
        let decor = item.as_value().map(|current| current.decor().clone());
        *item = Item::Value(new_value);
        if let Some(decor) = decor
            && let Some(value) = item.as_value_mut()
        {
            *value.decor_mut() = decor;
        }
        return;
    }
    table.insert(key, Item::Value(new_value));
}

fn reject_non_regular_config_path(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            std::io::Error::other(format!("用户配置路径 `{}` 不是普通文件", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{AppearancePreferences, MotionLevel, MuseConfigStore};
    use crate::domain::mcp::{McpProfileConfig, McpServerProfile};
    use crate::domain::skill::SkillPreferences;
    use std::collections::BTreeMap;

    fn unique_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "muse-config-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos()
        ))
    }

    #[test]
    fn creates_versioned_default_config() {
        let root = unique_root("default");
        let store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let content = std::fs::read_to_string(root.join("config.toml")).expect("应读取默认配置");
        assert!(content.contains("schema_version = 1"));
        assert!(!store.has_published_model_profiles());
        assert_eq!(store.snapshot().config.appearance.background_blur, 18);
        assert!(store.snapshot().diagnostics.is_empty());
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn provider_key_is_plaintext_in_protected_toml_but_never_in_public_snapshot() {
        let root = unique_root("provider-key");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        assert!(
            store
                .update_provider_api_key("deepseek", Some("plain-api-key".to_string()))
                .expect("应保存供应商 API Key")
        );

        let content = std::fs::read_to_string(root.join("config.toml")).expect("应读取配置");
        assert!(content.contains("api_key = \"plain-api-key\""));
        let public_snapshot = serde_json::to_string(&store.snapshot()).expect("快照应可序列化");
        assert!(!public_snapshot.contains("plain-api-key"));

        assert!(
            !store
                .update_provider_api_key("deepseek", None)
                .expect("应删除供应商 API Key")
        );
        let content = std::fs::read_to_string(root.join("config.toml")).expect("应回读配置");
        assert!(!content.contains("plain-api-key"));
        assert!(!content.contains("api_key ="));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    fn mcp_profiles_with_secret(secret: &str) -> McpProfileConfig {
        McpProfileConfig {
            mcp_servers: BTreeMap::from([(
                "github".to_string(),
                McpServerProfile {
                    transport: "stdio".to_string(),
                    enabled: true,
                    request_timeout_ms: Some(30_000),
                    enabled_tools: Some(vec!["search".to_string()]),
                    disabled_tools: Vec::new(),
                    approval_policy: Default::default(),
                    tool_approval_overrides: BTreeMap::new(),
                    command: Some("npx".to_string()),
                    args: Some(vec!["-y".to_string(), "github-mcp".to_string()]),
                    cwd: None,
                    url: None,
                    env: BTreeMap::from([("LOG_LEVEL".to_string(), "info".to_string())]),
                    secret_env: BTreeMap::from([("GITHUB_TOKEN".to_string(), secret.to_string())]),
                    headers: BTreeMap::new(),
                    secret_headers: BTreeMap::new(),
                },
            )]),
        }
    }

    #[test]
    fn mcp_api_key_is_plaintext_in_protected_toml_but_not_public_snapshot() {
        let root = unique_root("mcp-key");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        store
            .update_mcp_profiles(mcp_profiles_with_secret("mcp-plain-api-key"))
            .expect("应发布 MCP Server Profile");

        let content = std::fs::read_to_string(root.join("config.toml")).expect("应读取配置");
        assert!(content.contains("[mcp_servers.github]"));
        assert!(content.contains("[mcp_servers.github.secret_env]"));
        assert!(content.contains("GITHUB_TOKEN = \"mcp-plain-api-key\""));
        let public_snapshot = serde_json::to_string(&store.snapshot()).expect("快照应可序列化");
        assert!(!public_snapshot.contains("mcp-plain-api-key"));

        let runtime = store.mcp_runtime_snapshot();
        assert_eq!(
            runtime.runtime_entries()["github"]["env"]["GITHUB_TOKEN"],
            "mcp-plain-api-key"
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn preserves_manual_mcp_extensions_and_rejects_stale_page_save() {
        let root = unique_root("mcp-manual-edit");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        store
            .update_mcp_profiles(mcp_profiles_with_secret("first-key"))
            .expect("应发布 MCP Server Profile");
        let path = root.join("config.toml");
        let content = std::fs::read_to_string(&path).expect("应读取配置").replace(
            "[mcp_servers.github]\n",
            "[mcp_servers.github]\n# 用户 MCP 注释\nfuture_server_field = \"keep\"\n",
        );
        std::fs::write(&path, &content).expect("应模拟手工扩展 MCP 配置");

        let error = store
            .update_mcp_profiles(mcp_profiles_with_secret("must-not-write"))
            .expect_err("手工修改后必须拒绝陈旧页面保存");
        assert!(error.to_string().contains("应用外部修改"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("应回读手工配置"),
            content
        );

        assert!(!store.refresh_from_disk().expect("应确认已经刷新"));
        store
            .update_mcp_profiles(mcp_profiles_with_secret("fresh-key"))
            .expect("重新加载后应允许页面保存");
        let saved = std::fs::read_to_string(&path).expect("应读取保存结果");
        assert!(saved.contains("# 用户 MCP 注释"));
        assert!(saved.contains("future_server_field = \"keep\""));
        assert!(saved.contains("GITHUB_TOKEN = \"fresh-key\""));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn removing_mcp_secret_deletes_plaintext_from_toml() {
        let root = unique_root("mcp-secret-delete");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let mut profiles = mcp_profiles_with_secret("retired-key");
        store
            .update_mcp_profiles(profiles.clone())
            .expect("应发布初始 MCP API Key");
        profiles
            .mcp_servers
            .get_mut("github")
            .expect("应包含 github")
            .secret_env
            .clear();
        store
            .update_mcp_profiles(profiles)
            .expect("应删除 MCP API Key");

        let content = std::fs::read_to_string(root.join("config.toml")).expect("应回读配置");
        assert!(!content.contains("retired-key"));
        assert!(!content.contains("GITHUB_TOKEN"));
        assert!(!content.contains("[mcp_servers.github.secret_env]"));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn writes_agent_skill_overrides_in_standard_toml_layout() {
        let root = unique_root("skill-overrides");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let mut preferences = SkillPreferences::default();
        preferences.set_enabled("git-release", false);
        store
            .update_skill_preferences(preferences.clone())
            .expect("应发布 Skill 启停覆盖");

        let content = std::fs::read_to_string(root.join("config.toml")).expect("应读取配置");
        assert!(content.contains("[[skills.config]]"));
        assert!(content.contains("path = \"skills/git-release/SKILL.md\""));
        assert!(content.contains("enabled = false"));
        let reloaded = MuseConfigStore::load_from_dir(&root).expect("应回读配置");
        assert_eq!(reloaded.skill_preferences(), &preferences);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn preserves_manual_skill_extensions_and_rejects_stale_save() {
        let root = unique_root("skill-manual-edit");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let mut preferences = SkillPreferences::default();
        preferences.set_enabled("git-release", false);
        store
            .update_skill_preferences(preferences.clone())
            .expect("应发布 Skill 启停覆盖");
        let path = root.join("config.toml");
        let content = std::fs::read_to_string(&path)
            .expect("应读取配置")
            .replace("[skills]\n", "[skills]\nfuture_field = \"keep\"\n")
            .replace(
                "path = \"skills/git-release/SKILL.md\"\n",
                "path = \"skills/git-release/SKILL.md\"\nentry_note = \"keep\"\n",
            );
        std::fs::write(&path, &content).expect("应模拟手工编辑 Skill 配置");

        let error = store
            .update_skill_preferences(preferences.clone())
            .expect_err("陈旧页面必须拒绝覆盖手工配置");
        assert!(error.to_string().contains("应用外部修改"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);

        assert!(!store.refresh_from_disk().expect("应确认已刷新"));
        preferences.set_enabled("git-release", true);
        store
            .update_skill_preferences(preferences)
            .expect("刷新后应允许保存");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("future_field = \"keep\"")
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("entry_note = \"keep\"")
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn preserves_manual_provider_extensions_and_rejects_stale_page_save() {
        let root = unique_root("provider-manual-edit");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        store
            .update_provider_api_key("deepseek", None)
            .expect("应先发布默认 Provider Profile");
        let path = root.join("config.toml");
        let content = std::fs::read_to_string(&path).expect("应读取配置").replace(
            "[providers.deepseek]\n",
            "[providers.deepseek]\n# 用户供应商注释\nfuture_provider_field = \"keep\"\n",
        );
        std::fs::write(&path, &content).expect("应模拟手工扩展配置");

        let error = store
            .update_provider_api_key("deepseek", Some("must-not-write".to_string()))
            .expect_err("手工修改后必须拒绝陈旧页面保存");
        assert!(error.to_string().contains("应用外部修改"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("应回读手工配置"),
            content
        );

        assert!(!store.refresh_from_disk().expect("应确认已是最新 revision"));
        store
            .update_provider_api_key("deepseek", Some("fresh-page-key".to_string()))
            .expect("重新加载后应允许页面保存");
        let saved = std::fs::read_to_string(&path).expect("应读取保存结果");
        assert!(saved.contains("# 用户供应商注释"));
        assert!(saved.contains("future_provider_field = \"keep\""));
        assert!(saved.contains("api_key = \"fresh-page-key\""));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_repairs_config_with_current_user_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_root("permissions");
        let path = root.join("config.toml");
        let store = MuseConfigStore::load_from_dir(&root).expect("应创建受保护配置");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("应读取配置权限")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(store);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("应注入过宽权限");
        MuseConfigStore::load_from_dir(&root).expect("加载时应修复配置权限");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("应回读修复后权限")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn preserves_comments_order_and_unknown_fields_when_saving_appearance() {
        let root = unique_root("preserve");
        std::fs::create_dir_all(&root).expect("应创建测试目录");
        std::fs::write(
            root.join("config.toml"),
            r#"# 用户注释
schema_version = 1
custom_flag = "keep"

[appearance]
# 模糊注释
background_blur = 8
background_opacity = 0.8
motion_level = "reduced"
theme = "dark"
language = "zh-CN"
future_field = 42

[conversation]
send_key = "enter"
restore_last_session = true

[voice]
auto_play = false
input_language = "zh"

[updates]
check_on_startup = true
"#,
        )
        .expect("应写入测试配置");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应加载带扩展的配置");
        assert_eq!(store.snapshot().diagnostics.len(), 2);
        store
            .update_appearance(AppearancePreferences {
                theme: "system".to_string(),
                language: "zh-CN".to_string(),
                background_blur: 22,
                background_opacity: 0.6,
                motion_level: MotionLevel::None,
            })
            .expect("应保存外观偏好");
        let content = std::fs::read_to_string(root.join("config.toml")).expect("应读取保存结果");
        assert!(content.starts_with("# 用户注释"));
        assert!(content.contains("# 模糊注释"));
        assert!(content.contains("custom_flag = \"keep\""));
        assert!(content.contains("future_field = 42"));
        assert!(content.contains("background_blur = 22"));
        assert!(content.contains("motion_level = \"none\""));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn reports_field_diagnostics_without_overwriting_invalid_source() {
        let root = unique_root("diagnostics");
        std::fs::create_dir_all(&root).expect("应创建测试目录");
        let content = r#"schema_version = 1
[appearance]
theme = "system"
language = "zh-CN"
background_blur = "many"
background_opacity = 4.0
motion_level = "fast"
"#;
        std::fs::write(root.join("config.toml"), content).expect("应写入损坏字段配置");
        let store = MuseConfigStore::load_from_dir(&root).expect("字段错误应降级为诊断");
        let snapshot = store.snapshot();
        assert_eq!(snapshot.config.appearance.background_blur, 18);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|item| item.field_path == "appearance.background_blur")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("config.toml")).expect("应回读原文件"),
            content,
            "只读加载不得覆盖用户原始配置"
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn rejects_invalid_update_without_mutating_file() {
        let root = unique_root("rollback");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let before = std::fs::read(root.join("config.toml")).expect("应读取原配置");
        let mut invalid = store.snapshot().config.appearance;
        invalid.background_opacity = 1.5;
        let error = store
            .update_appearance(invalid)
            .expect_err("越界值必须拒绝");
        assert!(error.to_string().contains("背景可见度"));
        assert_eq!(
            std::fs::read(root.join("config.toml")).expect("应读取失败后的配置"),
            before
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn rejects_stale_page_save_after_external_config_edit() {
        let root = unique_root("external-edit");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let path = root.join("config.toml");
        let content = std::fs::read_to_string(&path)
            .expect("应读取配置")
            .replace("background_blur = 18", "background_blur = 9");
        std::fs::write(&path, &content).expect("应模拟手工编辑配置");

        let mut stale_appearance = store.snapshot().config.appearance;
        stale_appearance.background_blur = 22;
        let error = store
            .update_appearance(stale_appearance)
            .expect_err("外部修改后必须拒绝陈旧页面保存");
        assert!(error.to_string().contains("应用外部修改"));
        assert_eq!(store.snapshot().config.appearance.background_blur, 9);
        assert_eq!(
            std::fs::read_to_string(&path).expect("应回读手工配置"),
            content,
            "冲突不得覆盖手工配置"
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn rejects_malformed_toml_without_overwriting_source() {
        let root = unique_root("malformed");
        std::fs::create_dir_all(&root).expect("应创建测试目录");
        let content = b"schema_version = 1\n[appearance\nbackground_blur = 12\n";
        std::fs::write(root.join("config.toml"), content).expect("应写入语法损坏配置");

        let error =
            MuseConfigStore::load_from_dir(&root).expect_err("TOML 语法损坏必须阻止启动配置发布");
        assert!(error.to_string().contains("TOML 解析失败"));
        assert_eq!(
            std::fs::read(root.join("config.toml")).expect("应回读损坏源文件"),
            content,
            "解析失败不得覆盖用户原始配置"
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn refuses_to_replace_non_table_appearance_content() {
        let root = unique_root("inline-appearance");
        std::fs::create_dir_all(&root).expect("应创建测试目录");
        let content = r#"schema_version = 1
appearance = { future_field = "keep" }

[conversation]
send_key = "enter"
restore_last_session = true

[voice]
auto_play = false
input_language = "zh"

[updates]
check_on_startup = true
"#;
        std::fs::write(root.join("config.toml"), content).expect("应写入内联外观配置");
        let mut store = MuseConfigStore::load_from_dir(&root).expect("应以诊断方式加载");
        let error = store
            .update_appearance(AppearancePreferences::default())
            .expect_err("保存不得覆盖未知的非表外观内容");
        assert!(error.to_string().contains("本次保存已取消"));
        assert_eq!(
            std::fs::read_to_string(root.join("config.toml")).expect("应回读原文件"),
            content
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }
}
