//! 旧模型 JSON、SQLite 目录和系统凭据到 `config.toml` 的一次性迁移。

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::app::preferences::{MuseConfigStore, MuseConfigStoreError};
use crate::app::secret::{SecretStoreBackend, SecretStoreError};
use crate::app::storage::atomic_write_synced;
use crate::model::catalog::{LegacyModelCatalogStore, ModelCatalogError, ModelProviderCatalog};
use crate::model::config::store::{model_secret_identity, read_model_secret_binding};
use crate::model::config::{
    ModelConfigStore, ModelConfigStoreError, ModelSecretBinding, ModelSecretSlot, ModelsConfig,
};
use crate::model::profile_config::{
    ModelProfileConfig, ProviderModelConfig, ProviderProfileConfig,
};

const MODEL_MIGRATION_MARKER: &str = "migrations/model-config-v1.json";

#[derive(Debug)]
pub enum ModelProfileMigrationError {
    Config(MuseConfigStoreError),
    LegacyConfig(ModelConfigStoreError),
    Catalog(ModelCatalogError),
    Secret(SecretStoreError),
    Io(std::io::Error),
}

impl std::fmt::Display for ModelProfileMigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(_) => formatter.write_str("发布 config.toml Provider Profile 失败。"),
            Self::LegacyConfig(_) => formatter.write_str("读取旧模型配置失败。"),
            Self::Catalog(_) => formatter.write_str("读取旧模型目录失败。"),
            Self::Secret(_) => formatter.write_str("迁移旧模型凭据失败。"),
            Self::Io(_) => formatter.write_str("发布模型迁移标记失败。"),
        }
    }
}

impl std::error::Error for ModelProfileMigrationError {}

impl From<MuseConfigStoreError> for ModelProfileMigrationError {
    fn from(value: MuseConfigStoreError) -> Self {
        Self::Config(value)
    }
}

impl From<ModelConfigStoreError> for ModelProfileMigrationError {
    fn from(value: ModelConfigStoreError) -> Self {
        Self::LegacyConfig(value)
    }
}

impl From<ModelCatalogError> for ModelProfileMigrationError {
    fn from(value: ModelCatalogError) -> Self {
        Self::Catalog(value)
    }
}

impl From<SecretStoreError> for ModelProfileMigrationError {
    fn from(value: SecretStoreError) -> Self {
        Self::Secret(value)
    }
}

impl From<std::io::Error> for ModelProfileMigrationError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Serialize)]
struct MigrationMarker<'a> {
    schema: &'a str,
    source: &'a str,
}

/// 在启动运行时前完成模型事实源迁移和旧系统凭据清理。
///
/// Marker 只会在 TOML 回读成功且旧凭据已经幂等删除后发布。删除失败会中止启动，
/// 保留旧 SQLite 表和凭据，让下一次启动从同一安全点重试。
pub fn migrate_model_profiles<S: SecretStoreBackend>(
    base_dir: &Path,
    config_store: &mut MuseConfigStore,
    legacy_store: &mut ModelConfigStore,
    secrets: &S,
) -> Result<bool, ModelProfileMigrationError> {
    let marker_path = base_dir.join(MODEL_MIGRATION_MARKER);
    if marker_path.is_file() {
        return Ok(false);
    }
    if !config_store.model_profiles_are_valid() {
        return Err(
            MuseConfigStoreError::Validation(crate::app::preferences::ConfigDiagnostic {
                code: "config_model_profiles_invalid".to_string(),
                field_path: "providers".to_string(),
                message: "Provider Profile 无法解析；修正 config.toml 前不会迁移或清理旧凭据。"
                    .to_string(),
            })
            .into(),
        );
    }

    let legacy_database_existed = base_dir.join("runtime/muse.sqlite").is_file()
        || base_dir.join("runtime/agent-vp.sqlite").is_file();
    let legacy_sources_exist = legacy_store.storage_exists() || legacy_database_existed;
    if !legacy_sources_exist {
        if !config_store.has_published_model_profiles() {
            config_store.publish_migrated_model_profiles(ModelProfileConfig::default())?;
        }
        publish_marker(&marker_path)?;
        return Ok(true);
    }

    let should_import_legacy = !config_store.has_published_model_profiles();

    let catalog_store = LegacyModelCatalogStore::load_from_dir(base_dir)?;
    let catalog = catalog_store.catalog()?;
    let legacy_plaintext = legacy_store.config().clone();
    legacy_store.hydrate_api_keys_from_secret_store(secrets)?;
    restore_legacy_plaintext_api_keys(legacy_store, &legacy_plaintext);

    let mut profiles = if should_import_legacy {
        profiles_from_legacy_catalog(
            &catalog.providers,
            &catalog.models,
            legacy_store.config(),
            secrets,
        )?
    } else {
        config_store.model_profiles().clone()
    };

    if should_import_legacy {
        profiles
            .apply_runtime_config(legacy_store.config())
            .map_err(ModelProfileMigrationError::Catalog)?;
        config_store.publish_migrated_model_profiles(profiles)?;

        // 必须从刚发布的文件重新加载并核对，不能只相信进程内对象。
        let reloaded = MuseConfigStore::load_from_dir(base_dir)?;
        if !reloaded.has_published_model_profiles() {
            return Err(ModelProfileMigrationError::Io(std::io::Error::other(
                "Provider Profile 发布后回读缺失",
            )));
        }
    }

    cleanup_legacy_model_credentials(legacy_store.config(), &catalog.providers, secrets)?;
    publish_marker(&marker_path)?;
    Ok(true)
}

/// 旧 JSON 明文本身就是迁移来源。系统凭据回读用于补齐无明文配置，不能在找不到
/// 旧 account 时把 JSON 中仍然有效的 Key 擦掉。
fn restore_legacy_plaintext_api_keys(
    legacy_store: &mut ModelConfigStore,
    plaintext_source: &ModelsConfig,
) {
    let mut hydrated = legacy_store.config().clone();
    restore_missing_key(&mut hydrated.chat.api_key, &plaintext_source.chat.api_key);
    restore_missing_key(&mut hydrated.tts.api_key, &plaintext_source.tts.api_key);
    restore_missing_key(
        &mut hydrated.speech_recognition.api_key,
        &plaintext_source.speech_recognition.api_key,
    );
    restore_missing_key(
        &mut hydrated.audio_understanding.api_key,
        &plaintext_source.audio_understanding.api_key,
    );
    legacy_store.replace(hydrated);
}

fn restore_missing_key(target: &mut Option<String>, source: &Option<String>) {
    if target.is_none() {
        *target = source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
    }
}

fn profiles_from_legacy_catalog<S: SecretStoreBackend>(
    providers: &[ModelProviderCatalog],
    models: &[crate::model::catalog::ModelCatalogItem],
    legacy: &ModelsConfig,
    secrets: &S,
) -> Result<ModelProfileConfig, ModelProfileMigrationError> {
    let mut result = ModelProfileConfig::default();
    for provider in providers {
        let api_key = legacy_provider_api_key(provider, legacy, secrets)?
            .or_else(|| (!provider.api_key.trim().is_empty()).then(|| provider.api_key.clone()));
        let entry = result
            .providers
            .entry(provider.id.clone())
            .or_insert_with(|| ProviderProfileConfig {
                kind: provider.id.clone(),
                display_name: provider.name.clone(),
                base_url: provider.default_api_base.clone(),
                api_key: None,
                enabled: provider.enabled,
                notes: provider.notes.clone(),
                chat_model_list_url: provider.chat_model_list_url.clone(),
                tts_model_list_url: provider.tts_model_list_url.clone(),
                models: Default::default(),
            });
        entry.kind = provider.id.clone();
        entry.display_name = provider.name.clone();
        entry.base_url = provider.default_api_base.clone();
        entry.api_key = api_key;
        entry.enabled = provider.enabled;
        entry.notes = provider.notes.clone();
        entry.chat_model_list_url = provider.chat_model_list_url.clone();
        entry.tts_model_list_url = provider.tts_model_list_url.clone();
    }

    for model in models {
        let Some(provider) = result.providers.get_mut(&model.provider_id) else {
            continue;
        };
        provider.models.insert(
            model.model.clone(),
            ProviderModelConfig {
                display_name: model.name.clone(),
                modality: model
                    .functions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "chat".to_string()),
                enabled: model.enabled,
                notes: model.notes.clone(),
                tags: model.tags.clone(),
                functions: model.functions.clone(),
                context_window: model.context_window,
                default_max_output_tokens: model.default_max_output_tokens,
                supports_usage: model.supports_usage,
                supports_cached_tokens: model.supports_cached_tokens,
                supports_reasoning_tokens: model.supports_reasoning_tokens,
                tokenizer_family: model.tokenizer_family.clone(),
                temperature: 0.7,
                voice_id: String::new(),
                speed: 1.0,
                response_format: String::new(),
                language: String::new(),
            },
        );
    }
    Ok(result)
}

fn legacy_provider_api_key<S: SecretStoreBackend>(
    provider: &ModelProviderCatalog,
    legacy: &ModelsConfig,
    secrets: &S,
) -> Result<Option<String>, ModelProfileMigrationError> {
    if provider.credential_account.trim().is_empty() {
        return Ok(None);
    }
    let binding = ModelSecretBinding {
        account: provider.credential_account.clone(),
        identity: provider.credential_identity.clone(),
    };
    let mut identity_config = legacy.clone();
    identity_config.chat.provider = provider.id.clone();
    identity_config.chat.api_base = provider.default_api_base.clone();
    identity_config.chat.api_protocol = "chat_completions".to_string();
    let identity = model_secret_identity(&identity_config, ModelSecretSlot::Chat);
    read_model_secret_binding(secrets, ModelSecretSlot::Chat, &binding, &identity)
        .map_err(Into::into)
}

fn cleanup_legacy_model_credentials<S: SecretStoreBackend>(
    legacy: &ModelsConfig,
    providers: &[ModelProviderCatalog],
    secrets: &S,
) -> Result<(), ModelProfileMigrationError> {
    let mut accounts = BTreeSet::new();
    for slot in [
        ModelSecretSlot::Chat,
        ModelSecretSlot::Tts,
        ModelSecretSlot::Asr,
        ModelSecretSlot::AudioUnderstanding,
    ] {
        accounts.insert(slot.legacy_account().to_string());
        if let Some(binding) = legacy.secret_bindings.get(slot) {
            accounts.insert(binding.account.clone());
        }
    }
    accounts.extend(legacy.secret_bindings.pending_accounts.iter().cloned());
    accounts.extend(legacy.secret_bindings.retired_accounts.iter().cloned());
    accounts.extend(
        providers
            .iter()
            .map(|provider| provider.credential_account.trim())
            .filter(|account| !account.is_empty())
            .map(ToString::to_string),
    );
    for account in accounts {
        secrets.delete(&account)?;
    }
    Ok(())
}

fn publish_marker(path: &Path) -> Result<(), ModelProfileMigrationError> {
    let marker = serde_json::to_vec_pretty(&MigrationMarker {
        schema: "muse.model-config-migration/v1",
        source: "models/config.json + runtime/muse.sqlite + system-credential-store",
    })
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    atomic_write_synced(path, &marker)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::migrate_model_profiles;
    use crate::app::preferences::MuseConfigStore;
    use crate::app::secret::{SecretStoreBackend, SecretStoreError};
    use crate::model::config::store::{encode_model_secret_envelope, model_secret_identity};
    use crate::model::config::{
        ModelConfigStore, ModelSecretBinding, ModelSecretSlot, ModelsConfig,
    };
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSecrets {
        values: Mutex<HashMap<String, String>>,
        reads: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
        fail_delete_accounts: Mutex<HashSet<String>>,
    }

    impl FakeSecrets {
        fn with_value(account: &str, value: &str) -> Self {
            Self {
                values: Mutex::new(HashMap::from([(account.to_string(), value.to_string())])),
                ..Self::default()
            }
        }

        fn value(&self, account: &str) -> Option<String> {
            self.values.lock().ok()?.get(account).cloned()
        }

        fn fail_delete(&self, account: &str) {
            self.fail_delete_accounts
                .lock()
                .expect("失败注入锁")
                .insert(account.to_string());
        }

        fn allow_delete(&self, account: &str) {
            self.fail_delete_accounts
                .lock()
                .expect("失败注入锁")
                .remove(account);
        }
    }

    impl SecretStoreBackend for FakeSecrets {
        fn get_optional(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
            self.reads
                .lock()
                .map_err(|_| SecretStoreError("测试读取日志锁已损坏".to_string()))?
                .push(account.to_string());
            self.values
                .lock()
                .map(|values| values.get(account).cloned())
                .map_err(|_| SecretStoreError("测试凭据锁已损坏".to_string()))
        }

        fn set(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
            self.values
                .lock()
                .map_err(|_| SecretStoreError("测试凭据锁已损坏".to_string()))?
                .insert(account.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
            self.deletes
                .lock()
                .map_err(|_| SecretStoreError("测试删除日志锁已损坏".to_string()))?
                .push(account.to_string());
            if self
                .fail_delete_accounts
                .lock()
                .map_err(|_| SecretStoreError("测试失败注入锁已损坏".to_string()))?
                .contains(account)
            {
                return Err(SecretStoreError("模拟旧凭据删除失败".to_string()));
            }
            self.values
                .lock()
                .map_err(|_| SecretStoreError("测试凭据锁已损坏".to_string()))?
                .remove(account);
            Ok(())
        }
    }

    fn unique_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "muse-model-migration-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos()
        ))
    }

    fn write_foundation_config(root: &std::path::Path) {
        std::fs::create_dir_all(root).expect("应创建测试目录");
        std::fs::write(
            root.join("config.toml"),
            r#"schema_version = 1

[appearance]
theme = "system"
language = "zh-CN"
background_blur = 18
background_opacity = 1.0
motion_level = "full"
"#,
        )
        .expect("应写入基础配置");
    }

    #[test]
    fn fresh_profile_config_publishes_marker_without_touching_system_credentials() {
        let root = unique_root("fresh");
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应创建新配置");
        let mut legacy = ModelConfigStore::load_from_dir(&root).expect("应读取空旧配置");
        let secrets = FakeSecrets::default();

        assert!(
            migrate_model_profiles(&root, &mut config, &mut legacy, &secrets)
                .expect("新目录应完成空迁移")
        );
        assert!(root.join("migrations/model-config-v1.json").is_file());
        assert!(config.has_published_model_profiles());
        let content = std::fs::read_to_string(root.join("config.toml")).expect("应读取新配置");
        assert!(content.contains("[providers.deepseek]"));
        assert!(content.contains("enabled = false"));
        assert!(content.contains("[providers.deepseek.models.deepseek-v4-flash]"));
        assert!(content.contains("[active_models.chat]"));
        assert!(!content.contains("providers = {"));
        assert!(secrets.reads.lock().expect("读取日志锁").is_empty());
        assert!(secrets.deletes.lock().expect("删除日志锁").is_empty());
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn newly_created_default_config_does_not_hide_legacy_json() {
        let root = unique_root("legacy-without-config-toml");
        let mut legacy_config = ModelsConfig::default();
        legacy_config.chat.provider = "deepseek".to_string();
        legacy_config.chat.api_base = "https://api.deepseek.com".to_string();
        legacy_config.chat.api_key = Some("legacy-plaintext-key".to_string());
        legacy_config.chat.model = "deepseek-v4-pro".to_string();
        let legacy_path = root.join("models/config.json");
        std::fs::create_dir_all(legacy_path.parent().expect("旧配置应有父目录"))
            .expect("应创建旧模型目录");
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&legacy_config).expect("应序列化旧配置"),
        )
        .expect("应写入旧配置");
        let original_json = std::fs::read(&legacy_path).expect("应读取旧配置原文");

        let mut config = MuseConfigStore::load_from_dir(&root).expect("应新建基础配置");
        assert!(!config.has_published_model_profiles());
        let mut legacy = ModelConfigStore::load_from_dir(&root).expect("应加载旧模型配置");
        migrate_model_profiles(&root, &mut config, &mut legacy, &FakeSecrets::default())
            .expect("默认配置不得遮蔽旧模型事实");

        let reloaded = MuseConfigStore::load_from_dir(&root).expect("应回读迁移配置");
        assert_eq!(reloaded.chat().model, "deepseek-v4-pro");
        assert_eq!(
            reloaded.chat().api_key.as_deref(),
            Some("legacy-plaintext-key")
        );
        assert_eq!(
            std::fs::read(&legacy_path).expect("应保留旧配置"),
            original_json
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn migrates_keyring_and_catalog_to_toml_then_drops_sqlite_config_tables() {
        const ACCOUNT: &str = "model.chat.v2.0123456789abcdef0123456789abcdef";
        let root = unique_root("legacy-keyring");
        write_foundation_config(&root);
        let mut legacy_config = ModelsConfig::default();
        legacy_config.chat.provider = "deepseek".to_string();
        legacy_config.chat.api_base = "https://api.deepseek.com".to_string();
        legacy_config.chat.model = "deepseek-v4-pro".to_string();
        let identity = model_secret_identity(&legacy_config, ModelSecretSlot::Chat);
        legacy_config.secret_bindings.chat = Some(ModelSecretBinding {
            account: ACCOUNT.to_string(),
            identity: identity.clone(),
        });
        let legacy_path = root.join("models/config.json");
        std::fs::create_dir_all(legacy_path.parent().expect("旧配置应有父目录"))
            .expect("应创建旧模型目录");
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&legacy_config).expect("应序列化旧配置"),
        )
        .expect("应写入旧模型配置");
        let original_json = std::fs::read(&legacy_path).expect("应读取旧配置原文");
        let envelope =
            encode_model_secret_envelope(&identity, "legacy-secret-value").expect("应封装旧凭据");
        let secrets = FakeSecrets::with_value(ACCOUNT, &envelope);
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应加载基础配置");
        let mut legacy = ModelConfigStore::load_from_dir(&root).expect("应加载旧模型配置");

        migrate_model_profiles(&root, &mut config, &mut legacy, &secrets)
            .expect("应完成旧模型迁移");
        let reloaded = MuseConfigStore::load_from_dir(&root).expect("应回读新配置");
        assert_eq!(
            reloaded
                .model_profiles()
                .providers
                .get("deepseek")
                .and_then(|provider| provider.api_key.as_deref()),
            Some("legacy-secret-value")
        );
        let toml = std::fs::read_to_string(root.join("config.toml")).expect("应读取 TOML");
        assert!(toml.contains("api_key = \"legacy-secret-value\""));
        assert_eq!(
            std::fs::read(&legacy_path).expect("应保留旧配置"),
            original_json
        );
        assert!(secrets.value(ACCOUNT).is_none());

        let (_, database) = crate::app::storage::open_runtime_database(&root)
            .expect("Provider Profile 发布后应能迁移数据库");
        let parallel_tables: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('providers', 'models')",
                [],
                |row| row.get(0),
            )
            .expect("应统计旧配置表");
        assert_eq!(parallel_tables, 0);
        drop(database);

        let before = std::fs::read(root.join("config.toml")).expect("应读取迁移后配置");
        let mut second_config = MuseConfigStore::load_from_dir(&root).expect("应重复加载配置");
        let mut second_legacy = ModelConfigStore::load_from_dir(&root).expect("应重复加载旧配置");
        assert!(
            !migrate_model_profiles(&root, &mut second_config, &mut second_legacy, &secrets)
                .expect("重复启动应幂等")
        );
        assert_eq!(
            std::fs::read(root.join("config.toml")).expect("应读取重复启动配置"),
            before
        );
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn credential_cleanup_failure_retries_after_toml_publish_without_losing_key() {
        const ACCOUNT: &str = "model.chat.v2.22222222222222222222222222222222";
        let root = unique_root("credential-cleanup-retry");
        write_foundation_config(&root);
        let mut legacy_config = ModelsConfig::default();
        legacy_config.chat.provider = "deepseek".to_string();
        legacy_config.chat.api_base = "https://api.deepseek.com".to_string();
        legacy_config.chat.model = "deepseek-v4-pro".to_string();
        let identity = model_secret_identity(&legacy_config, ModelSecretSlot::Chat);
        legacy_config.secret_bindings.chat = Some(ModelSecretBinding {
            account: ACCOUNT.to_string(),
            identity: identity.clone(),
        });
        std::fs::create_dir_all(root.join("models")).expect("应创建旧模型目录");
        std::fs::write(
            root.join("models/config.json"),
            serde_json::to_vec_pretty(&legacy_config).expect("应序列化旧配置"),
        )
        .expect("应写入旧配置");
        let envelope =
            encode_model_secret_envelope(&identity, "retry-secret").expect("应封装旧凭据");
        let secrets = FakeSecrets::with_value(ACCOUNT, &envelope);
        secrets.fail_delete(ACCOUNT);
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应加载基础配置");
        let mut legacy = ModelConfigStore::load_from_dir(&root).expect("应加载旧配置");

        let error = migrate_model_profiles(&root, &mut config, &mut legacy, &secrets)
            .expect_err("旧凭据清理失败时不得发布 marker");
        assert!(!error.to_string().contains("retry-secret"));
        assert!(secrets.value(ACCOUNT).is_some());
        assert!(!root.join("migrations/model-config-v1.json").exists());
        assert!(
            std::fs::read_to_string(root.join("config.toml"))
                .expect("TOML 应已安全发布")
                .contains("api_key = \"retry-secret\"")
        );

        secrets.allow_delete(ACCOUNT);
        let mut retry_config = MuseConfigStore::load_from_dir(&root).expect("应重载 TOML");
        let mut retry_legacy = ModelConfigStore::load_from_dir(&root).expect("应重载旧配置");
        migrate_model_profiles(&root, &mut retry_config, &mut retry_legacy, &secrets)
            .expect("下一次启动应完成幂等清理");
        assert!(secrets.value(ACCOUNT).is_none());
        assert!(root.join("migrations/model-config-v1.json").is_file());
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn external_edit_conflict_keeps_old_credential_and_skips_marker() {
        const ACCOUNT: &str = "model.chat.v2.11111111111111111111111111111111";
        let root = unique_root("conflict");
        write_foundation_config(&root);
        let mut legacy_config = ModelsConfig::default();
        legacy_config.chat.provider = "deepseek".to_string();
        legacy_config.chat.api_base = "https://api.deepseek.com".to_string();
        legacy_config.chat.model = "deepseek-v4-pro".to_string();
        let identity = model_secret_identity(&legacy_config, ModelSecretSlot::Chat);
        legacy_config.secret_bindings.chat = Some(ModelSecretBinding {
            account: ACCOUNT.to_string(),
            identity: identity.clone(),
        });
        std::fs::create_dir_all(root.join("models")).expect("应创建旧模型目录");
        std::fs::write(
            root.join("models/config.json"),
            serde_json::to_vec_pretty(&legacy_config).expect("应序列化旧配置"),
        )
        .expect("应写入旧配置");
        let envelope =
            encode_model_secret_envelope(&identity, "keep-on-conflict").expect("应封装旧凭据");
        let secrets = FakeSecrets::with_value(ACCOUNT, &envelope);
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应加载基础配置");
        let mut legacy = ModelConfigStore::load_from_dir(&root).expect("应加载旧配置");
        let mut externally_edited =
            std::fs::read_to_string(root.join("config.toml")).expect("应读取配置");
        externally_edited.push_str("\n# 用户并发手工编辑\n");
        std::fs::write(root.join("config.toml"), externally_edited).expect("应写入外部修改");

        assert!(
            migrate_model_profiles(&root, &mut config, &mut legacy, &secrets).is_err(),
            "外部 revision 变化必须拒绝迁移覆盖"
        );
        assert!(secrets.value(ACCOUNT).is_some());
        assert!(!root.join("migrations/model-config-v1.json").exists());
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn malformed_provider_profile_never_cleans_legacy_credential() {
        let root = unique_root("malformed-provider-profile");
        write_foundation_config(&root);
        let mut content =
            std::fs::read_to_string(root.join("config.toml")).expect("应读取基础配置");
        content.push_str(
            r#"
[providers.deepseek]
kind = "deepseek"
api_key = 123

[active_models]
voice_input_mode = "speech_text"
"#,
        );
        std::fs::write(root.join("config.toml"), content).expect("应写入损坏 Provider Profile");
        std::fs::create_dir_all(root.join("models")).expect("应创建旧模型目录");
        std::fs::write(
            root.join("models/config.json"),
            serde_json::to_vec_pretty(&ModelsConfig::default()).expect("应序列化旧配置"),
        )
        .expect("应写入旧配置");
        let secrets = FakeSecrets::with_value("model.chat", "legacy-key-must-stay");
        let mut config = MuseConfigStore::load_from_dir(&root).expect("损坏字段应进入诊断");
        assert!(!config.model_profiles_are_valid());
        let mut legacy = ModelConfigStore::load_from_dir(&root).expect("应加载旧配置");

        assert!(
            migrate_model_profiles(&root, &mut config, &mut legacy, &secrets).is_err(),
            "损坏 Provider Profile 必须阻断迁移"
        );
        assert_eq!(
            secrets.value("model.chat").as_deref(),
            Some("legacy-key-must-stay")
        );
        assert!(!root.join("migrations/model-config-v1.json").exists());
        assert!(secrets.deletes.lock().expect("删除日志锁").is_empty());
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }
}
