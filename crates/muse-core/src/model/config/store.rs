//! 旧模型 JSON 兼容模块，只供一次性迁移读取和历史迁移测试。

#[cfg(test)]
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    LlmConfig, ModelSecretBinding, ModelSecretSlot, ModelsConfig, SpeechRecognitionConfig,
    TtsConfig,
};
use crate::app::secret::{SecretStoreBackend, SecretStoreError};
use crate::model::profile::provider_profile_for_identity;

const MODEL_CONFIG_STORE_DIR: &str = "models";
const MODEL_CONFIG_STORE_FILE: &str = "config.json";
const MODEL_SECRET_ENVELOPE_SCHEMA: &str = "muse.model-secret/v2";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ModelSecretEnvelope {
    schema: String,
    identity: String,
    secret: String,
}

/// 模型配置存储读写错误。
#[derive(Debug)]
pub enum ModelConfigStoreError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Secret(SecretStoreError),
}

impl std::fmt::Display for ModelConfigStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelConfigStoreError::Io(err) => write!(f, "模型配置存储读写失败：{err}"),
            ModelConfigStoreError::Serde(err) => write!(f, "模型配置存储序列化失败：{err}"),
            ModelConfigStoreError::Secret(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ModelConfigStoreError {}

impl From<std::io::Error> for ModelConfigStoreError {
    fn from(value: std::io::Error) -> Self {
        ModelConfigStoreError::Io(value)
    }
}

impl From<serde_json::Error> for ModelConfigStoreError {
    fn from(value: serde_json::Error) -> Self {
        ModelConfigStoreError::Serde(value)
    }
}

impl From<SecretStoreError> for ModelConfigStoreError {
    fn from(value: SecretStoreError) -> Self {
        ModelConfigStoreError::Secret(value)
    }
}

/// 旧模型配置本地存储。
///
/// 读取 `<数据目录>/models/config.json` 中的聊天、语音合成、语音识别和音频理解
/// 配置。新运行时不得调用 `save` 回写该文件；稳态事实源是 `config.toml`。
///
/// 遵循与 `PersonaStore`、`VisualPackStore` 一致的加载/保存模式，
/// 包括 BOM 剥离与目录自动创建。
#[derive(Debug, Clone)]
pub struct ModelConfigStore {
    storage_path: PathBuf,
    config: ModelsConfig,
    has_pending_migration: bool,
}

impl ModelConfigStore {
    /// 从指定基础目录加载模型配置；若文件不存在，则返回默认空配置。
    pub fn load_from_dir(base_dir: impl AsRef<Path>) -> Result<Self, ModelConfigStoreError> {
        let base_dir = base_dir.as_ref();
        let storage_path = Self::storage_path(base_dir);

        if !storage_path.exists() {
            return Ok(Self {
                storage_path,
                config: ModelsConfig::default(),
                has_pending_migration: false,
            });
        }

        let content = std::fs::read_to_string(&storage_path)?;
        let normalized = normalize_json_text(&content);
        if normalized.trim().is_empty() {
            return Ok(Self {
                storage_path,
                config: ModelsConfig::default(),
                has_pending_migration: false,
            });
        }

        let mut config: ModelsConfig = serde_json::from_str(normalized)?;
        let has_pending_migration = normalize_loaded_config(&mut config);

        Ok(Self {
            storage_path,
            config,
            has_pending_migration,
        })
    }

    /// 以 JSON 文件形式持久化当前模型配置。
    pub fn save(&mut self) -> Result<(), ModelConfigStoreError> {
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 即使调用方仍在内存中持有密钥，写盘时也必须强制剥离，避免任何新版本
        // 再次把凭据写回配置文件。
        let mut persistable = self.config.clone();
        clear_api_keys(&mut persistable);
        let content = serde_json::to_string_pretty(&persistable)?;
        atomic_write_synced(&self.storage_path, content.as_bytes())?;
        self.has_pending_migration = false;
        Ok(())
    }

    /// 聚合配置快照（只读）。
    pub fn config(&self) -> &ModelsConfig {
        &self.config
    }

    /// 返回聊天主模型配置。
    pub fn chat(&self) -> &LlmConfig {
        &self.config.chat
    }

    /// 返回语音合成配置。
    pub fn tts(&self) -> &TtsConfig {
        &self.config.tts
    }

    /// 返回语音识别配置。
    pub fn speech_recognition(&self) -> &SpeechRecognitionConfig {
        &self.config.speech_recognition
    }

    /// 返回音频理解模型配置。
    pub fn audio_understanding(&self) -> &LlmConfig {
        &self.config.audio_understanding
    }

    /// 返回语音输入策略配置。
    pub fn voice_input(&self) -> &super::VoiceInputConfig {
        &self.config.voice_input
    }

    /// 返回外部 MCP 服务配置映射。
    pub fn mcp_servers(&self) -> &std::collections::BTreeMap<String, Value> {
        &self.config.mcp_servers
    }

    /// 整体替换三段配置。调用方负责随后调用 `save()` 持久化。
    pub fn replace(&mut self, mut config: ModelsConfig) {
        config.tts.ensure_default_profiles();
        config.speech_recognition.ensure_openai_compatible();
        self.config = config;
        // 不清 has_pending_migration：只有 save() 真正写盘后才能声称迁移落地。
    }

    /// 仅替换聊天段配置。调用方负责随后调用 `save()`。
    #[allow(dead_code)]
    pub fn set_chat(&mut self, chat: LlmConfig) {
        self.config.chat = chat;
        // 同 replace()：内存改动不能假装迁移已落盘。
    }

    /// 仅替换语音合成段配置。调用方负责随后调用 `save()`。
    #[allow(dead_code)]
    pub fn set_tts(&mut self, mut tts: TtsConfig) {
        tts.ensure_default_profiles();
        self.config.tts = tts;
    }

    /// 仅替换语音识别配置。调用方负责随后调用 `save()`。
    #[allow(dead_code)]
    pub fn set_speech_recognition(&mut self, mut speech_recognition: SpeechRecognitionConfig) {
        speech_recognition.ensure_openai_compatible();
        self.config.speech_recognition = speech_recognition;
    }

    /// 持久化文件是否存在于磁盘（区分首启与已落盘）。
    pub fn storage_exists(&self) -> bool {
        self.storage_path.exists()
    }

    /// 返回指定数据目录下的模型配置文件路径。
    pub fn storage_path_for_dir(base_dir: impl AsRef<Path>) -> PathBuf {
        Self::storage_path(base_dir.as_ref())
    }

    /// 是否存在待回写的结构迁移结果。
    pub fn has_pending_migration(&self) -> bool {
        self.has_pending_migration
    }

    /// 在加载完成后，用 `LLM_API_KEY` 环境变量为缺省密钥的段补填。
    ///
    /// 保留开发者用环境变量免填 key 的便利：Store 从磁盘加载后，
    /// 若聊天段或音频理解段的 `api_key` 为空且环境变量存在，则自动补填。
    pub fn apply_env_api_key_fallback(&mut self) {
        if let Ok(key) = std::env::var("LLM_API_KEY") {
            if self.config.chat.api_key.is_none() {
                self.config.chat.api_key = Some(key.clone());
            }
            if self.config.audio_understanding.api_key.is_none() {
                self.config.audio_understanding.api_key = Some(key);
            }
        }
        if let Ok(key) = std::env::var("TTS_API_KEY")
            && self.config.tts.api_key.is_none()
        {
            self.config.tts.api_key = Some(key);
        }
    }

    /// 将旧配置文件中的明文密钥迁移到系统凭据库，并从内存配置中移除旧副本。
    pub fn migrate_plaintext_api_keys<S: SecretStoreBackend>(
        &mut self,
        secrets: &S,
    ) -> Result<bool, ModelConfigStoreError> {
        let mut changed = false;
        for slot in MODEL_SECRET_SLOTS {
            changed |= migrate_plaintext_api_key(&mut self.config, secrets, slot)?;
        }
        Ok(changed)
    }

    /// 用系统凭据库中的密钥补齐运行时内存配置，供 provider 创建与连通性检查使用。
    ///
    /// 没有 v2 引用的旧配置会把固定 account 中的密钥复制到唯一 account，再把
    /// 新引用写入内存；调用方必须随后以 `save()` 原子落盘。身份不匹配的引用会
    /// 被清除，绝不会把原 endpoint 的密钥注入被手工改写后的新 endpoint。
    pub fn hydrate_api_keys_from_secret_store<S: SecretStoreBackend>(
        &mut self,
        secrets: &S,
    ) -> Result<bool, ModelConfigStoreError> {
        let mut changed = false;
        for slot in MODEL_SECRET_SLOTS {
            changed |= hydrate_api_key(&mut self.config, secrets, slot)?;
        }
        Ok(changed)
    }

    /// 清理配置切换前崩溃遗留的暂存 account。
    ///
    /// pending account 尚未被任何生效配置引用；仍额外排除当前 binding，避免损坏
    /// 或手工编辑的日志误删正在使用的凭据。
    pub fn recover_pending_api_keys<S: SecretStoreBackend>(
        &mut self,
        secrets: &S,
    ) -> Result<bool, ModelConfigStoreError> {
        let pending = self.config.secret_bindings.pending_accounts.clone();
        if pending.is_empty() {
            return Ok(false);
        }
        let active_accounts = MODEL_SECRET_SLOTS
            .iter()
            .filter_map(|slot| self.config.secret_bindings.get(*slot))
            .map(|binding| binding.account.as_str())
            .collect::<Vec<_>>();
        for account in &pending {
            if is_model_secret_cleanup_account(account)
                && !active_accounts.contains(&account.as_str())
            {
                secrets.delete(account)?;
            }
        }
        self.config.secret_bindings.pending_accounts.clear();
        Ok(true)
    }

    /// 幂等清理由已提交配置记录的旧凭据 account。
    ///
    /// 清理列表不含密钥明文。若进程在配置切换后、删除旧凭据前崩溃，下次启动
    /// 会重试这里；只有受控的 v2 模型 account 才允许被删除。
    pub fn recover_retired_api_keys<S: SecretStoreBackend>(
        &mut self,
        secrets: &S,
    ) -> Result<bool, ModelConfigStoreError> {
        let retired = self.config.secret_bindings.retired_accounts.clone();
        if retired.is_empty() {
            return Ok(false);
        }
        let active_accounts = MODEL_SECRET_SLOTS
            .iter()
            .filter_map(|slot| self.config.secret_bindings.get(*slot))
            .map(|binding| binding.account.as_str())
            .collect::<Vec<_>>();
        for account in &retired {
            if is_model_secret_cleanup_account(account)
                && !active_accounts.contains(&account.as_str())
            {
                secrets.delete(account)?;
            }
        }
        self.config.secret_bindings.retired_accounts.clear();
        Ok(true)
    }

    fn storage_path(base_dir: &Path) -> PathBuf {
        base_dir
            .join(MODEL_CONFIG_STORE_DIR)
            .join(MODEL_CONFIG_STORE_FILE)
    }
}

const MODEL_SECRET_SLOTS: [ModelSecretSlot; 4] = [
    ModelSecretSlot::Chat,
    ModelSecretSlot::Tts,
    ModelSecretSlot::Asr,
    ModelSecretSlot::AudioUnderstanding,
];

/// 计算密钥与 provider endpoint 的稳定身份。模型名和采样参数不属于凭据边界。
pub fn model_secret_identity(config: &ModelsConfig, slot: ModelSecretSlot) -> String {
    let (provider, api_base, protocol) = match slot {
        ModelSecretSlot::Chat => (
            config.chat.provider.as_str(),
            config.chat.api_base.as_str(),
            config.chat.api_protocol.as_str(),
        ),
        ModelSecretSlot::Tts => (
            config.tts.provider.as_str(),
            config.tts.api_base.as_str(),
            "tts",
        ),
        ModelSecretSlot::Asr => (
            config.speech_recognition.provider.as_str(),
            config.speech_recognition.api_base.as_str(),
            "asr",
        ),
        ModelSecretSlot::AudioUnderstanding => (
            config.audio_understanding.provider.as_str(),
            config.audio_understanding.api_base.as_str(),
            config.audio_understanding.api_protocol.as_str(),
        ),
    };
    let provider = provider.trim().to_ascii_lowercase();
    let api_base = api_base.trim().trim_end_matches('/');
    let protocol = protocol.trim().to_ascii_lowercase();
    format!(
        "model-secret/v2|{}:{provider}|{}:{api_base}|{}:{protocol}",
        provider.len(),
        api_base.len(),
        protocol.len()
    )
}

/// 为一次密钥替换生成新的不可复用凭据引用。
pub fn new_model_secret_binding(
    slot: ModelSecretSlot,
    identity: String,
) -> Result<ModelSecretBinding, ModelConfigStoreError> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| std::io::Error::other(format!("生成模型凭据引用失败：{error}")))?;
    let mut suffix = String::with_capacity(nonce.len() * 2);
    for byte in nonce {
        use std::fmt::Write as _;
        write!(suffix, "{byte:02x}").expect("写入 String 不会失败");
    }
    Ok(ModelSecretBinding {
        account: format!("{}{suffix}", slot.account_prefix()),
        identity,
    })
}

/// 将模型密钥封装为带 endpoint 身份的 v2 凭据值。
///
/// 配置文件中的 binding 只用于定位 account 和辅助诊断；hydrate 的授权依据是
/// 此 envelope 内的身份与当前配置实时计算出的身份相等。
pub fn encode_model_secret_envelope(
    identity: &str,
    secret: &str,
) -> Result<String, ModelConfigStoreError> {
    Ok(serde_json::to_string(&ModelSecretEnvelope {
        schema: MODEL_SECRET_ENVELOPE_SCHEMA.to_string(),
        identity: identity.to_string(),
        secret: secret.to_string(),
    })?)
}

/// 从指定 v2 凭据引用读取密钥，并同时校验 account 命名空间和 endpoint 身份。
///
/// 该入口供供应商级凭据切换复用；任何引用或 envelope 身份不一致都会按未配置
/// 处理，避免把一个供应商的密钥注入另一个 endpoint。
pub fn read_model_secret_binding<S: SecretStoreBackend>(
    secrets: &S,
    slot: ModelSecretSlot,
    binding: &ModelSecretBinding,
    expected_identity: &str,
) -> Result<Option<String>, ModelConfigStoreError> {
    if !is_managed_v2_model_secret_account_for_slot(&binding.account, slot)
        || binding.identity != expected_identity
    {
        return Ok(None);
    }
    let Some(value) = secrets.get_optional(&binding.account)? else {
        return Ok(None);
    };
    let Some(envelope) = decode_model_secret_envelope(&value) else {
        return Ok(None);
    };
    if envelope.identity != binding.identity || envelope.identity != expected_identity {
        return Ok(None);
    }
    let secret = envelope.secret.trim();
    Ok((!secret.is_empty()).then(|| secret.to_string()))
}

/// 判断 account 是否严格属于本应用的 v2 模型凭据命名空间。
pub fn is_managed_model_secret_account(account: &str) -> bool {
    MODEL_SECRET_SLOTS
        .iter()
        .any(|slot| is_managed_v2_model_secret_account_for_slot(account, *slot))
}

/// 判断 account 是否为升级前的固定模型凭据引用。
pub fn is_legacy_model_secret_account(account: &str) -> bool {
    MODEL_SECRET_SLOTS
        .iter()
        .any(|slot| account == slot.legacy_account())
}

fn is_model_secret_cleanup_account(account: &str) -> bool {
    is_managed_model_secret_account(account) || is_legacy_model_secret_account(account)
}

fn is_managed_v2_model_secret_account_for_slot(account: &str, slot: ModelSecretSlot) -> bool {
    account
        .strip_prefix(slot.account_prefix())
        .is_some_and(|suffix| {
            suffix.len() == 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn decode_model_secret_envelope(value: &str) -> Option<ModelSecretEnvelope> {
    let envelope = serde_json::from_str::<ModelSecretEnvelope>(value).ok()?;
    (envelope.schema == MODEL_SECRET_ENVELOPE_SCHEMA).then_some(envelope)
}

fn migrate_plaintext_api_key<S: SecretStoreBackend>(
    config: &mut ModelsConfig,
    secrets: &S,
    slot: ModelSecretSlot,
) -> Result<bool, ModelConfigStoreError> {
    let Some(value) = take_api_key(config, slot).filter(|value| !value.trim().is_empty()) else {
        return Ok(false);
    };
    let identity = model_secret_identity(config, slot);
    let envelope = encode_model_secret_envelope(&identity, &value)?;
    let binding = new_model_secret_binding(slot, identity)?;
    if let Err(error) = secrets.set_verified(&binding.account, &envelope) {
        let _ = secrets.delete(&binding.account);
        set_api_key(config, slot, Some(value));
        return Err(error.into());
    }
    config.secret_bindings.set(slot, Some(binding));
    retire_account(config, slot.legacy_account());
    Ok(true)
}

fn hydrate_api_key<S: SecretStoreBackend>(
    config: &mut ModelsConfig,
    secrets: &S,
    slot: ModelSecretSlot,
) -> Result<bool, ModelConfigStoreError> {
    let identity = model_secret_identity(config, slot);
    let binding = config.secret_bindings.get(slot).cloned();
    if let Some(mut binding) = binding {
        if is_managed_v2_model_secret_account_for_slot(&binding.account, slot)
            && let Some(value) = secrets.get_optional(&binding.account)?
            && let Some(envelope) = decode_model_secret_envelope(&value)
            && envelope.identity == identity
        {
            let changed = binding.identity != identity;
            binding.identity = identity;
            config.secret_bindings.set(slot, Some(binding));
            set_api_key(config, slot, Some(envelope.secret));
            return Ok(changed);
        }
        if is_managed_v2_model_secret_account_for_slot(&binding.account, slot) {
            retire_account(config, &binding.account);
        }
        // 即使 fixed account 尚未成功删除，也必须用持久化 tombstone 阻断回灌。
        retire_account(config, slot.legacy_account());
        config.secret_bindings.set(slot, None);
        set_api_key(config, slot, None);
        return Ok(true);
    }

    if has_legacy_account_tombstone(config, slot) {
        set_api_key(config, slot, None);
        return Ok(false);
    }

    let Some(value) = secrets.get_optional(slot.legacy_account())? else {
        set_api_key(config, slot, None);
        return Ok(false);
    };
    let envelope = encode_model_secret_envelope(&identity, &value)?;
    let binding = new_model_secret_binding(slot, identity)?;
    if let Err(error) = secrets.set_verified(&binding.account, &envelope) {
        let _ = secrets.delete(&binding.account);
        return Err(error.into());
    }
    config.secret_bindings.set(slot, Some(binding));
    retire_account(config, slot.legacy_account());
    set_api_key(config, slot, Some(value));
    Ok(true)
}

fn has_legacy_account_tombstone(config: &ModelsConfig, slot: ModelSecretSlot) -> bool {
    let legacy_account = slot.legacy_account();
    config
        .secret_bindings
        .pending_accounts
        .iter()
        .chain(config.secret_bindings.retired_accounts.iter())
        .any(|account| account == legacy_account)
}

fn retire_account(config: &mut ModelsConfig, account: &str) {
    if !config
        .secret_bindings
        .retired_accounts
        .iter()
        .any(|candidate| candidate == account)
    {
        config
            .secret_bindings
            .retired_accounts
            .push(account.to_string());
    }
}

fn take_api_key(config: &mut ModelsConfig, slot: ModelSecretSlot) -> Option<String> {
    match slot {
        ModelSecretSlot::Chat => config.chat.api_key.take(),
        ModelSecretSlot::Tts => config.tts.api_key.take(),
        ModelSecretSlot::Asr => config.speech_recognition.api_key.take(),
        ModelSecretSlot::AudioUnderstanding => config.audio_understanding.api_key.take(),
    }
}

fn set_api_key(config: &mut ModelsConfig, slot: ModelSecretSlot, value: Option<String>) {
    match slot {
        ModelSecretSlot::Chat => config.chat.api_key = value,
        ModelSecretSlot::Tts => config.tts.api_key = value,
        ModelSecretSlot::Asr => config.speech_recognition.api_key = value,
        ModelSecretSlot::AudioUnderstanding => config.audio_understanding.api_key = value,
    }
}

fn clear_api_keys(config: &mut ModelsConfig) {
    config.chat.api_key = None;
    config.audio_understanding.api_key = None;
    config.tts.api_key = None;
    config.speech_recognition.api_key = None;
}

/// 将配置字节原子发布到目标路径，供其他本地事实存储复用同一耐久写入语义。
pub fn atomic_write_synced(path: &Path, content: &[u8]) -> Result<(), std::io::Error> {
    crate::app::storage::atomic_write_synced(path, content)
}

#[cfg(test)]
fn atomic_write_synced_with_hook<F>(
    path: &Path,
    content: &[u8],
    before_replace: F,
) -> Result<(), std::io::Error>
where
    F: FnOnce() -> Result<(), std::io::Error>,
{
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("模型配置路径缺少父目录"))?;
    std::fs::create_dir_all(parent)?;
    let (temporary_path, mut temporary_file) = create_unique_temporary_file(path)?;

    let result = (|| {
        temporary_file.write_all(content)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        before_replace()?;
        replace_config_path(&temporary_path, path)?;
        sync_parent_directory(parent);
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(test)]
fn create_unique_temporary_file(path: &Path) -> Result<(PathBuf, File), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("模型配置路径缺少父目录"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    for _ in 0..16 {
        let mut nonce = [0_u8; 12];
        getrandom::fill(&mut nonce)
            .map_err(|error| std::io::Error::other(format!("生成临时文件名失败：{error}")))?;
        let suffix = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let temporary_path = parent.join(format!(".{file_name}.{suffix}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "无法创建唯一的模型配置临时文件",
    ))
}

#[cfg(test)]
fn replace_config_path(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    crate::app::storage::replace_file(source, destination)
}

#[cfg(all(test, unix))]
fn sync_parent_directory(parent: &Path) {
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        // rename 已经提交；此时回滚或删除新凭据反而会让磁盘配置引用不存在的密钥。
        // 部分文件系统不支持目录 fsync，因此记录告警并保持已提交状态。
        tracing::warn!(path = %parent.display(), %error, "模型配置已原子替换，但父目录同步失败");
    }
}

#[cfg(all(test, not(unix)))]
fn sync_parent_directory(_parent: &Path) {}

fn normalize_json_text(content: &str) -> &str {
    content.trim_start_matches('\u{feff}')
}

fn normalize_loaded_config(config: &mut ModelsConfig) -> bool {
    let mut changed = false;

    changed |= config.tts.ensure_default_profiles();
    changed |= config.speech_recognition.ensure_openai_compatible();
    changed |= config.voice_input.normalize_supported_mode();
    for section in [&mut config.chat, &mut config.audio_understanding] {
        if section.api_protocol != "chat_completions" {
            section.api_protocol = "chat_completions".to_string();
            changed = true;
        }
    }

    if let Some(profile) =
        provider_profile_for_identity(&config.chat.provider, &config.chat.api_base)
    {
        let maximum = profile.model_defaults.default_max_output_tokens;
        if config.chat.max_tokens == 0 || config.chat.max_tokens > maximum {
            config.chat.max_tokens = maximum;
            changed = true;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::{
        ModelConfigStore, atomic_write_synced_with_hook, encode_model_secret_envelope,
        read_model_secret_binding,
    };
    use crate::app::secret::{SecretStoreBackend, SecretStoreError};
    use crate::model::config::{ModelSecretBinding, ModelSecretSlot, ModelsConfig};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct FakeSecretStore {
        values: Mutex<HashMap<String, String>>,
        operations: Mutex<Vec<String>>,
    }

    impl FakeSecretStore {
        fn with_value(account: &str, value: &str) -> Self {
            Self {
                values: Mutex::new(HashMap::from([(account.to_string(), value.to_string())])),
                operations: Mutex::new(Vec::new()),
            }
        }

        fn value(&self, account: &str) -> Option<String> {
            self.values.lock().ok()?.get(account).cloned()
        }
    }

    impl SecretStoreBackend for FakeSecretStore {
        fn get_optional(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
            self.values
                .lock()
                .map(|values| values.get(account).cloned())
                .map_err(|_| SecretStoreError("测试凭据存储锁已损坏".to_string()))
        }

        fn set(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
            self.operations
                .lock()
                .map_err(|_| SecretStoreError("测试凭据操作锁已损坏".to_string()))?
                .push(format!("set:{account}"));
            self.values
                .lock()
                .map_err(|_| SecretStoreError("测试凭据存储锁已损坏".to_string()))?
                .insert(account.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
            self.operations
                .lock()
                .map_err(|_| SecretStoreError("测试凭据操作锁已损坏".to_string()))?
                .push(format!("delete:{account}"));
            self.values
                .lock()
                .map_err(|_| SecretStoreError("测试凭据存储锁已损坏".to_string()))?
                .remove(account);
            Ok(())
        }
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("muse-model-config-{nanos}-{counter}"))
    }

    fn loads_default_when_file_missing() {
        let dir = unique_temp_dir();
        let store = ModelConfigStore::load_from_dir(&dir).expect("缺失文件应返回默认配置");
        assert_eq!(store.chat().provider, "");
        assert_eq!(store.tts().runtime_engine(), "openai_audio_speech");
        assert!(!store.speech_recognition().enabled);
    }

    fn saves_and_reloads_config() {
        let dir = unique_temp_dir();
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("加载初始配置");
        {
            let mut chat = store.chat().clone();
            chat.provider = "openai".to_string();
            chat.model = "gpt-4o".to_string();
            store.set_chat(chat);
        }
        store.save().expect("保存配置");

        let reloaded = ModelConfigStore::load_from_dir(&dir).expect("重新加载");
        assert_eq!(reloaded.chat().provider, "openai");
        assert_eq!(reloaded.chat().model, "gpt-4o");
    }

    fn save_never_persists_api_keys() {
        let dir = unique_temp_dir();
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("加载初始配置");
        let mut config = ModelsConfig::default();
        config.chat.api_key = Some("chat-secret-value".to_string());
        config.audio_understanding.api_key = Some("audio-secret-value".to_string());
        config.tts.api_key = Some("tts-secret-value".to_string());
        store.replace(config);
        store.save().expect("保存配置");

        let content = std::fs::read_to_string(dir.join("models").join("config.json"))
            .expect("读取已保存配置");
        assert!(!content.contains("chat-secret-value"));
        assert!(!content.contains("audio-secret-value"));
        assert!(!content.contains("tts-secret-value"));
    }

    fn loading_agent_plan_clamps_inherited_output_limit() {
        let dir = unique_temp_dir();
        let target = dir.join("models").join("config.json");
        std::fs::create_dir_all(target.parent().unwrap()).expect("应创建模型配置目录");
        let mut config = ModelsConfig::default();
        config.chat.provider = "volcengine_agent_plan".to_string();
        config.chat.api_base = "https://ark.cn-beijing.volces.com/api/plan/v3".to_string();
        config.chat.model = "doubao-seed-2.0-pro".to_string();
        config.chat.max_tokens = 1_000_000;
        std::fs::write(
            &target,
            serde_json::to_vec_pretty(&config).expect("配置应可序列化"),
        )
        .expect("应写入旧配置");

        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载并修正旧配置");
        assert_eq!(store.chat().max_tokens, 2048);
        assert!(store.has_pending_migration());
        store.save().expect("应保存修正后的配置");

        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(target).expect("应读取修正后的配置"))
                .expect("配置应为 JSON");
        assert_eq!(persisted["chat"]["max_tokens"], 2048);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn loads_store_with_utf8_bom() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("models").join("config.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        // 写入带 UTF-8 BOM 前缀的配置，模拟 Windows 编辑器手改场景。
        let content = serde_json::to_string_pretty(&ModelsConfig::default()).unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(content.as_bytes());
        std::fs::write(&target, bytes).unwrap();

        let store = ModelConfigStore::load_from_dir(&dir).expect("应能跳过 BOM 加载");
        assert_eq!(store.chat().provider, "");
    }

    fn loading_unimplemented_voice_input_mode_migrates_to_speech_text() {
        let dir = unique_temp_dir();
        let target = dir.join("models").join("config.json");
        std::fs::create_dir_all(target.parent().unwrap()).expect("应创建模型配置目录");
        let mut config = ModelsConfig::default();
        config.voice_input.mode = "audio_understanding".to_string();
        std::fs::write(
            &target,
            serde_json::to_vec_pretty(&config).expect("配置应可序列化"),
        )
        .expect("应写入旧语音输入配置");

        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载并迁移旧配置");
        assert_eq!(store.voice_input().mode, "speech_text");
        assert!(store.has_pending_migration());
        store.save().expect("迁移结果应可持久化");
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(target).expect("应读取迁移后的配置"))
                .expect("迁移后的配置应为 JSON");
        assert_eq!(persisted["voice_input"]["mode"], "speech_text");

        let _ = std::fs::remove_dir_all(dir);
    }

    fn atomic_save_crash_before_replace_keeps_old_config_readable() {
        let dir = unique_temp_dir();
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载模型配置");
        let mut old = ModelsConfig::default();
        old.chat.provider = "old-provider".to_string();
        store.replace(old);
        store.save().expect("应保存旧配置");
        let target = ModelConfigStore::storage_path_for_dir(&dir);
        let new_content = serde_json::to_vec_pretty(&ModelsConfig::default()).unwrap();

        let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ =
                atomic_write_synced_with_hook(&target, &new_content, || -> std::io::Result<()> {
                    panic!("模拟临时文件同步后进程崩溃")
                });
        }));

        assert!(crash.is_err());
        let reloaded = ModelConfigStore::load_from_dir(&dir).expect("旧配置必须仍可读取");
        assert_eq!(reloaded.chat().provider, "old-provider");
        assert!(
            std::fs::read_dir(target.parent().unwrap())
                .expect("应枚举模型目录")
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp")),
            "模拟崩溃应留下未发布的唯一临时文件"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn concurrent_atomic_saves_never_publish_truncated_json() {
        let dir = unique_temp_dir();
        let mut first = ModelConfigStore::load_from_dir(&dir).expect("应加载第一个存储");
        let mut first_config = ModelsConfig::default();
        first_config.chat.provider = "provider-a".repeat(2_000);
        first.replace(first_config);
        let mut second = ModelConfigStore::load_from_dir(&dir).expect("应加载第二个存储");
        let mut second_config = ModelsConfig::default();
        second_config.chat.provider = "provider-b".repeat(2_000);
        second.replace(second_config);
        let barrier = Arc::new(Barrier::new(3));

        let first_barrier = barrier.clone();
        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            first.save()
        });
        let second_barrier = barrier.clone();
        let second_thread = std::thread::spawn(move || {
            second_barrier.wait();
            second.save()
        });
        barrier.wait();
        first_thread
            .join()
            .expect("第一个保存线程不应 panic")
            .unwrap();
        second_thread
            .join()
            .expect("第二个保存线程不应 panic")
            .unwrap();

        let reloaded = ModelConfigStore::load_from_dir(&dir).expect("并发保存结果必须是完整 JSON");
        assert!(
            reloaded.chat().provider == "provider-a".repeat(2_000)
                || reloaded.chat().provider == "provider-b".repeat(2_000)
        );
        let temporary_files = std::fs::read_dir(dir.join("models"))
            .expect("应枚举模型目录")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temporary_files, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn provider_binding_reader_rejects_cross_endpoint_and_invalid_accounts() {
        let identity = "model-secret/v2|8:deepseek|24:https://api.deepseek.com|16:chat_completions";
        let account = "model.chat.v2.0123456789abcdef0123456789abcdef";
        let envelope =
            encode_model_secret_envelope(identity, "test-secret").expect("应能封装测试凭据");
        let secrets = FakeSecretStore::with_value(account, &envelope);
        let binding = ModelSecretBinding {
            account: account.to_string(),
            identity: identity.to_string(),
        };

        assert_eq!(
            read_model_secret_binding(&secrets, ModelSecretSlot::Chat, &binding, identity)
                .expect("应能读取匹配凭据"),
            Some("test-secret".to_string())
        );
        assert!(
            read_model_secret_binding(&secrets, ModelSecretSlot::Chat, &binding, "other-endpoint")
                .expect("身份不匹配应按未配置处理")
                .is_none()
        );
        let invalid_binding = ModelSecretBinding {
            account: "model.chat".to_string(),
            identity: identity.to_string(),
        };
        assert!(
            read_model_secret_binding(&secrets, ModelSecretSlot::Chat, &invalid_binding, identity)
                .expect("旧 account 不得作为供应商凭据引用")
                .is_none()
        );
    }

    fn startup_migrates_legacy_account_before_idempotent_cleanup() {
        let dir = unique_temp_dir();
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载模型配置");
        let mut config = ModelsConfig::default();
        config.chat.provider = "openai".to_string();
        config.chat.api_base = "https://api.example/v1".to_string();
        store.replace(config);
        store.save().expect("应保存没有 v2 引用的旧配置");
        let secrets = FakeSecretStore::with_value("model.chat", "legacy-chat-secret");

        assert!(
            store
                .hydrate_api_keys_from_secret_store(&secrets)
                .expect("应迁移固定 account")
        );
        let binding = store
            .config()
            .secret_bindings
            .chat
            .clone()
            .expect("迁移后必须生成 v2 引用");
        let stored = secrets.value(&binding.account).expect("v2 envelope 应存在");
        let envelope: serde_json::Value =
            serde_json::from_str(&stored).expect("应写入 JSON envelope");
        assert_eq!(envelope["schema"], "muse.model-secret/v2");
        assert_eq!(envelope["identity"], binding.identity);
        assert_eq!(envelope["secret"], "legacy-chat-secret");
        assert!(
            store
                .config()
                .secret_bindings
                .retired_accounts
                .contains(&"model.chat".to_string())
        );

        // 启动流程先发布新引用。这里即使立刻崩溃，下一次也只会读取 v2 account。
        store.save().expect("应先原子发布 v2 引用");
        let mut restarted = ModelConfigStore::load_from_dir(&dir).expect("应模拟重启加载");
        assert!(
            !restarted
                .hydrate_api_keys_from_secret_store(&secrets)
                .expect("重启应直接读取 v2 account")
        );
        assert_eq!(
            restarted.chat().api_key.as_deref(),
            Some("legacy-chat-secret")
        );
        assert!(
            restarted
                .recover_retired_api_keys(&secrets)
                .expect("应幂等清理固定 account")
        );
        restarted.save().expect("应清除已完成的清理日志");
        assert!(secrets.value("model.chat").is_none());
        assert_eq!(
            secrets.value(&binding.account).as_deref(),
            Some(stored.as_str())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn startup_rejects_tampered_endpoint_binding_and_account_using_envelope_identity() {
        const OLD_ACCOUNT: &str = "model.chat.v2.11111111111111111111111111111111";
        let dir = unique_temp_dir();
        let mut old = ModelsConfig::default();
        old.chat.provider = "openai".to_string();
        old.chat.api_base = "https://old.example/v1".to_string();
        let old_identity = super::model_secret_identity(&old, ModelSecretSlot::Chat);
        let old_envelope = super::encode_model_secret_envelope(&old_identity, "old-chat-secret")
            .expect("应封装旧密钥");
        old.secret_bindings.chat = Some(ModelSecretBinding {
            account: OLD_ACCOUNT.to_string(),
            identity: old_identity,
        });
        // 攻击者同时篡改 endpoint、配置中的诊断 identity，并继续指向旧 v2 account。
        old.chat.api_base = "https://new.example/v1".to_string();
        old.secret_bindings.chat.as_mut().unwrap().identity =
            super::model_secret_identity(&old, ModelSecretSlot::Chat);
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载模型配置");
        store.replace(old);
        store.save().expect("应保存身份不匹配夹具");
        let secrets = FakeSecretStore::with_value(OLD_ACCOUNT, &old_envelope);
        secrets
            .set("model.chat", "legacy-fixed-secret")
            .expect("应保留尚未清理的 fixed 密钥");
        let mut restarted = ModelConfigStore::load_from_dir(&dir).expect("应模拟重启加载");

        assert!(
            restarted
                .hydrate_api_keys_from_secret_store(&secrets)
                .expect("身份不匹配应被安全清除")
        );
        assert!(restarted.chat().api_key.is_none());
        assert!(restarted.config().secret_bindings.chat.is_none());
        assert!(
            restarted
                .config()
                .secret_bindings
                .retired_accounts
                .contains(&"model.chat".to_string()),
            "binding 不匹配后必须留下 fixed fallback tombstone"
        );

        // 模拟清理 fixed 前崩溃；重启只能看到 tombstone，不能从 fixed account 回灌。
        restarted.save().expect("应持久化安全 tombstone");
        let mut crashed_restart = ModelConfigStore::load_from_dir(&dir).expect("应再次重启");
        assert!(
            !crashed_restart
                .hydrate_api_keys_from_secret_store(&secrets)
                .expect("tombstone 应安全阻断 fixed fallback")
        );
        assert!(crashed_restart.chat().api_key.is_none());
        assert!(crashed_restart.config().secret_bindings.chat.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn startup_cleanup_never_deletes_any_active_binding_account() {
        const ACTIVE_ACCOUNT: &str = "model.chat.v2.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const ORPHAN_ACCOUNT: &str = "model.tts.v2.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let dir = unique_temp_dir();
        let mut config = ModelsConfig::default();
        config.chat.provider = "openai".to_string();
        config.chat.api_base = "https://api.example/v1".to_string();
        config.secret_bindings.chat = Some(ModelSecretBinding {
            account: ACTIVE_ACCOUNT.to_string(),
            identity: super::model_secret_identity(&config, ModelSecretSlot::Chat),
        });
        config.secret_bindings.pending_accounts =
            vec![ACTIVE_ACCOUNT.to_string(), ORPHAN_ACCOUNT.to_string()];
        config.secret_bindings.retired_accounts = vec![ACTIVE_ACCOUNT.to_string()];
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载模型配置");
        store.replace(config);
        store.save().expect("应保存切换前崩溃夹具");
        let secrets = FakeSecretStore::default();
        secrets.set(ACTIVE_ACCOUNT, "active-secret").unwrap();
        secrets.set(ORPHAN_ACCOUNT, "orphan-secret").unwrap();

        assert!(
            store
                .recover_pending_api_keys(&secrets)
                .expect("应恢复 pending account")
        );
        assert_eq!(
            secrets.value(ACTIVE_ACCOUNT).as_deref(),
            Some("active-secret")
        );
        assert!(secrets.value(ORPHAN_ACCOUNT).is_none());
        assert!(store.config().secret_bindings.pending_accounts.is_empty());
        assert!(
            store
                .recover_retired_api_keys(&secrets)
                .expect("应恢复 retired account")
        );
        assert_eq!(
            secrets.value(ACTIVE_ACCOUNT).as_deref(),
            Some("active-secret")
        );
        assert!(store.config().secret_bindings.retired_accounts.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn managed_account_requires_exact_prefix_and_lowercase_hex_suffix() {
        assert!(super::is_managed_model_secret_account(
            "model.chat.v2.0123456789abcdef0123456789abcdef"
        ));
        assert!(!super::is_managed_model_secret_account("model.chat"));
        assert!(super::is_legacy_model_secret_account("model.chat"));
        assert!(!super::is_managed_model_secret_account(
            "model.chat.v2.0123456789ABCDEF0123456789ABCDEF"
        ));
        assert!(!super::is_managed_model_secret_account(
            "model.chat.v2.0123456789abcdef"
        ));
        assert!(!super::is_managed_model_secret_account(
            "model.chat.v2.0123456789abcdef0123456789abcdef00"
        ));
        assert!(!super::is_managed_model_secret_account(
            "model.chat.v2.0123456789abcdef0123456789abcdeg"
        ));
    }

    fn legacy_fixed_fallback_is_blocked_by_pending_or_retired_tombstone() {
        for use_pending in [true, false] {
            let dir = unique_temp_dir();
            let mut config = ModelsConfig::default();
            config.chat.provider = "openai".to_string();
            config.chat.api_base = "https://api.example/v1".to_string();
            if use_pending {
                config
                    .secret_bindings
                    .pending_accounts
                    .push("model.chat".to_string());
            } else {
                config
                    .secret_bindings
                    .retired_accounts
                    .push("model.chat".to_string());
            }
            let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载配置");
            store.replace(config);
            let secrets = FakeSecretStore::with_value("model.chat", "legacy-fixed-secret");

            assert!(
                !store
                    .hydrate_api_keys_from_secret_store(&secrets)
                    .expect("tombstone 应阻断 fixed fallback")
            );
            assert!(store.chat().api_key.is_none());
            assert!(store.config().secret_bindings.chat.is_none());
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn raw_value_in_v2_account_is_never_hydrated() {
        const ACCOUNT: &str = "model.chat.v2.cccccccccccccccccccccccccccccccc";
        let dir = unique_temp_dir();
        let mut config = ModelsConfig::default();
        config.chat.provider = "openai".to_string();
        config.chat.api_base = "https://api.example/v1".to_string();
        config.secret_bindings.chat = Some(ModelSecretBinding {
            account: ACCOUNT.to_string(),
            identity: super::model_secret_identity(&config, ModelSecretSlot::Chat),
        });
        let mut store = ModelConfigStore::load_from_dir(&dir).expect("应加载配置");
        store.replace(config);
        let secrets = FakeSecretStore::with_value(ACCOUNT, "raw-v2-secret");

        assert!(
            store
                .hydrate_api_keys_from_secret_store(&secrets)
                .expect("裸 v2 值必须失效")
        );
        assert!(store.chat().api_key.is_none());
        assert!(store.config().secret_bindings.chat.is_none());
        assert!(
            store
                .config()
                .secret_bindings
                .retired_accounts
                .contains(&"model.chat".to_string())
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn aggregated_sync_test_cases() {
        let mut failures = Vec::new();
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loads_default_when_file_missing()
            }))
            .is_err()
            {
                failures.push("loads_default_when_file_missing");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(saves_and_reloads_config))
                .is_err()
            {
                failures.push("saves_and_reloads_config");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                save_never_persists_api_keys()
            }))
            .is_err()
            {
                failures.push("save_never_persists_api_keys");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loading_agent_plan_clamps_inherited_output_limit()
            }))
            .is_err()
            {
                failures.push("loading_agent_plan_clamps_inherited_output_limit");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loads_store_with_utf8_bom()
            }))
            .is_err()
            {
                failures.push("loads_store_with_utf8_bom");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loading_unimplemented_voice_input_mode_migrates_to_speech_text()
            }))
            .is_err()
            {
                failures.push("loading_unimplemented_voice_input_mode_migrates_to_speech_text");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                atomic_save_crash_before_replace_keeps_old_config_readable()
            }))
            .is_err()
            {
                failures.push("atomic_save_crash_before_replace_keeps_old_config_readable");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                concurrent_atomic_saves_never_publish_truncated_json()
            }))
            .is_err()
            {
                failures.push("concurrent_atomic_saves_never_publish_truncated_json");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provider_binding_reader_rejects_cross_endpoint_and_invalid_accounts()
            }))
            .is_err()
            {
                failures
                    .push("provider_binding_reader_rejects_cross_endpoint_and_invalid_accounts");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                startup_migrates_legacy_account_before_idempotent_cleanup()
            }))
            .is_err()
            {
                failures.push("startup_migrates_legacy_account_before_idempotent_cleanup");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                startup_rejects_tampered_endpoint_binding_and_account_using_envelope_identity()
            }))
            .is_err()
            {
                failures.push(
                    "startup_rejects_tampered_endpoint_binding_and_account_using_envelope_identity",
                );
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                startup_cleanup_never_deletes_any_active_binding_account()
            }))
            .is_err()
            {
                failures.push("startup_cleanup_never_deletes_any_active_binding_account");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                managed_account_requires_exact_prefix_and_lowercase_hex_suffix()
            }))
            .is_err()
            {
                failures.push("managed_account_requires_exact_prefix_and_lowercase_hex_suffix");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                legacy_fixed_fallback_is_blocked_by_pending_or_retired_tombstone()
            }))
            .is_err()
            {
                failures.push("legacy_fixed_fallback_is_blocked_by_pending_or_retired_tombstone");
            }
        }
        {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                raw_value_in_v2_account_is_never_hydrated()
            }))
            .is_err()
            {
                failures.push("raw_value_in_v2_account_is_never_hydrated");
            }
        }
        assert!(failures.is_empty(), "聚合测试失败：{}", failures.join(", "));
    }
}
