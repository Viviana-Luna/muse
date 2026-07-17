//! 旧 MCP JSON、环境引用和系统凭据到 `config.toml` 的一次性迁移。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::app::preferences::{ConfigDiagnostic, MuseConfigStore, MuseConfigStoreError};
use crate::app::secret::{SecretStoreBackend, SecretStoreError};
use crate::app::storage::atomic_write_synced;
use crate::domain::mcp::config::{McpProfileConfig, McpServerProfile};
use crate::domain::mcp::store::{PersistedMcpStore, legacy_mcp_store_path};

const MCP_MIGRATION_MARKER: &str = "migrations/mcp-config-v1.json";

#[derive(Debug)]
pub enum McpProfileMigrationError {
    Config(MuseConfigStoreError),
    Secret(SecretStoreError),
    Io(std::io::Error),
    Legacy(String),
}

impl std::fmt::Display for McpProfileMigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(_) => formatter.write_str("发布 config.toml MCP Server Profile 失败。"),
            Self::Secret(_) => formatter.write_str("迁移旧 MCP API Key 失败。"),
            Self::Io(_) => formatter.write_str("读取旧 MCP 配置或发布迁移标记失败。"),
            Self::Legacy(_) => formatter.write_str("旧 MCP 配置无法安全迁移。"),
        }
    }
}

impl std::error::Error for McpProfileMigrationError {}

impl From<MuseConfigStoreError> for McpProfileMigrationError {
    fn from(value: MuseConfigStoreError) -> Self {
        Self::Config(value)
    }
}

impl From<SecretStoreError> for McpProfileMigrationError {
    fn from(value: SecretStoreError) -> Self {
        Self::Secret(value)
    }
}

impl From<std::io::Error> for McpProfileMigrationError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Serialize)]
struct MigrationMarker<'a> {
    schema: &'a str,
    source: &'a str,
}

/// 在 MCP 运行时启动前发布 TOML，并在回读成功后清理旧 Keyring account。
pub fn migrate_mcp_profiles<S: SecretStoreBackend>(
    base_dir: &Path,
    config_store: &mut MuseConfigStore,
    legacy_model_servers: &BTreeMap<String, Value>,
    secrets: &S,
) -> Result<bool, McpProfileMigrationError> {
    let marker_path = base_dir.join(MCP_MIGRATION_MARKER);
    if marker_path.is_file() {
        return Ok(false);
    }
    if !config_store.mcp_profiles_are_valid() {
        return Err(MuseConfigStoreError::Validation(ConfigDiagnostic {
            code: "config_mcp_profiles_invalid".to_string(),
            field_path: "mcp_servers".to_string(),
            message: "MCP Server Profile 无法解析；修正 config.toml 前不会迁移或清理旧凭据。"
                .to_string(),
        })
        .into());
    }

    let legacy_servers = load_legacy_servers(base_dir, legacy_model_servers)?;
    let accounts = legacy_accounts(&legacy_servers)?;
    if !config_store.has_published_mcp_profiles() {
        let migrated = profiles_from_legacy(&legacy_servers, secrets)?;
        config_store.publish_migrated_mcp_profiles(migrated.clone())?;
        let reloaded = MuseConfigStore::load_from_dir(base_dir)?;
        if !reloaded.has_published_mcp_profiles()
            || !reloaded.mcp_profiles_are_valid()
            || reloaded.mcp_profiles() != &migrated
        {
            return Err(std::io::Error::other("MCP Server Profile 发布后回读失败").into());
        }
    }

    for account in accounts {
        secrets.delete(&account)?;
    }
    publish_marker(&marker_path)?;
    Ok(true)
}

fn load_legacy_servers(
    base_dir: &Path,
    legacy_model_servers: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, McpProfileMigrationError> {
    let path = legacy_mcp_store_path(base_dir);
    if path.is_file() {
        let content = std::fs::read_to_string(path)?;
        let parsed: PersistedMcpStore =
            serde_json::from_str(content.trim_start_matches('\u{feff}'))
                .map_err(|error| McpProfileMigrationError::Legacy(error.to_string()))?;
        if parsed.schema_version != 1 {
            return Err(McpProfileMigrationError::Legacy(
                "旧 MCP 配置 schema version 不受支持".to_string(),
            ));
        }
        let _legacy_revision = parsed.revision;
        return Ok(parsed.servers);
    }
    Ok(legacy_model_servers.clone())
}

fn profiles_from_legacy<S: SecretStoreBackend>(
    legacy_servers: &BTreeMap<String, Value>,
    secrets: &S,
) -> Result<McpProfileConfig, McpProfileMigrationError> {
    let mut output = McpProfileConfig::default();
    for (name, raw) in legacy_servers {
        let profile = profile_from_legacy(name, raw, secrets)?;
        output.mcp_servers.insert(name.clone(), profile);
    }
    output
        .validate()
        .map_err(McpProfileMigrationError::Legacy)?;
    Ok(output)
}

fn legacy_accounts(
    legacy_servers: &BTreeMap<String, Value>,
) -> Result<BTreeSet<String>, McpProfileMigrationError> {
    let mut accounts = BTreeSet::new();
    for raw in legacy_servers.values() {
        let object = raw.as_object().ok_or_else(|| {
            McpProfileMigrationError::Legacy("MCP server 配置不是对象".to_string())
        })?;
        accounts.extend(string_map(object.get("secret_bindings"))?.into_values());
    }
    Ok(accounts)
}

fn profile_from_legacy<S: SecretStoreBackend>(
    name: &str,
    raw: &Value,
    secrets: &S,
) -> Result<McpServerProfile, McpProfileMigrationError> {
    let object = raw
        .as_object()
        .ok_or_else(|| McpProfileMigrationError::Legacy("MCP server 配置不是对象".to_string()))?;
    let transport = transport_kind(object);
    let bindings = string_map(object.get("secret_bindings"))?;
    let enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let request_timeout_ms = legacy_timeout_ms(object)?;
    let enabled_tools = optional_string_list(object.get("enabled_tools"))?;
    let disabled_tools = string_list(object.get("disabled_tools"))?;

    let mut profile = McpServerProfile {
        transport: transport.clone(),
        enabled,
        request_timeout_ms,
        enabled_tools,
        disabled_tools,
        approval_policy: Default::default(),
        tool_approval_overrides: BTreeMap::new(),
        command: None,
        args: None,
        cwd: None,
        url: None,
        env: BTreeMap::new(),
        secret_env: BTreeMap::new(),
        headers: BTreeMap::new(),
        secret_headers: BTreeMap::new(),
    };

    match transport.as_str() {
        "stdio" => {
            profile.command = string_value(object.get("command"));
            profile.args = Some(string_list(object.get("args"))?);
            profile.cwd = string_value(object.get("cwd"));
            for (target, value) in string_map(object.get("env"))? {
                migrate_value(
                    target,
                    value,
                    &bindings,
                    secrets,
                    &mut profile.env,
                    &mut profile.secret_env,
                )?;
            }
            for environment_name in string_list(object.get("env_vars"))? {
                if profile.env.contains_key(&environment_name)
                    || profile.secret_env.contains_key(&environment_name)
                {
                    continue;
                }
                let value = resolve_reference(&environment_name, &bindings, secrets)?;
                profile.secret_env.insert(environment_name, value);
            }
        }
        "streamable_http" => {
            let url = string_value(object.get("url"));
            profile.url = url
                .map(|value| resolve_inline_reference(&value, &bindings, secrets))
                .transpose()?;
            let mut headers = string_map(object.get("headers"))?;
            headers.extend(string_map(object.get("http_headers"))?);
            for (target, environment_name) in string_map(object.get("env_http_headers"))? {
                headers.insert(target, format!("${{{environment_name}}}"));
            }
            for (target, value) in headers {
                migrate_value(
                    target,
                    value,
                    &bindings,
                    secrets,
                    &mut profile.headers,
                    &mut profile.secret_headers,
                )?;
            }
            if let Some(environment_name) = object
                .get("bearer_token_env_var")
                .or_else(|| object.get("bearer_token_env"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let value = resolve_reference(environment_name, &bindings, secrets)?;
                profile
                    .secret_headers
                    .insert("Authorization".to_string(), format!("Bearer {value}"));
            }
        }
        _ => {
            return Err(McpProfileMigrationError::Legacy(
                "旧 MCP transport 不受支持".to_string(),
            ));
        }
    }
    profile
        .validate(name)
        .map_err(McpProfileMigrationError::Legacy)?;
    Ok(profile)
}

fn migrate_value<S: SecretStoreBackend>(
    target: String,
    value: String,
    bindings: &BTreeMap<String, String>,
    secrets: &S,
    ordinary: &mut BTreeMap<String, String>,
    sensitive: &mut BTreeMap<String, String>,
) -> Result<(), McpProfileMigrationError> {
    let Some(reference) = environment_reference(&value) else {
        ordinary.insert(target, value);
        return Ok(());
    };
    let resolved = resolve_reference(reference, bindings, secrets)?;
    sensitive.insert(target, resolved);
    Ok(())
}

fn resolve_inline_reference<S: SecretStoreBackend>(
    value: &str,
    bindings: &BTreeMap<String, String>,
    secrets: &S,
) -> Result<String, McpProfileMigrationError> {
    match environment_reference(value) {
        Some(reference) => resolve_reference(reference, bindings, secrets),
        None => Ok(value.to_string()),
    }
}

fn resolve_reference<S: SecretStoreBackend>(
    environment_name: &str,
    bindings: &BTreeMap<String, String>,
    secrets: &S,
) -> Result<String, McpProfileMigrationError> {
    if let Some(account) = bindings.get(environment_name)
        && let Some(value) = secrets.get_optional(account)?
        && !value.trim().is_empty()
    {
        return Ok(value);
    }
    if let Ok(value) = std::env::var(environment_name)
        && !value.trim().is_empty()
    {
        return Ok(value);
    }
    Err(McpProfileMigrationError::Legacy(
        "MCP API Key 引用缺少可迁移值".to_string(),
    ))
}

fn environment_reference(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
        .or_else(|| trimmed.strip_prefix('$'))
        .filter(|value| !value.is_empty())
}

fn transport_kind(object: &serde_json::Map<String, Value>) -> String {
    object
        .get("type")
        .and_then(Value::as_str)
        .map(|value| match value {
            "http" | "http_json_rpc" => "streamable_http",
            other => other,
        })
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            if object.contains_key("url") {
                "streamable_http".to_string()
            } else {
                "stdio".to_string()
            }
        })
}

fn legacy_timeout_ms(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<u64>, McpProfileMigrationError> {
    for key in ["request_timeout_ms", "timeout_ms", "startup_timeout_ms"] {
        if let Some(value) = object.get(key) {
            let timeout = value
                .as_u64()
                .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
                .ok_or_else(|| {
                    McpProfileMigrationError::Legacy("旧 MCP 毫秒超时字段无效".to_string())
                })?;
            return Ok(Some(timeout));
        }
    }
    for key in ["tool_timeout_sec", "startup_timeout_sec"] {
        if let Some(value) = object.get(key) {
            let seconds = value
                .as_f64()
                .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| {
                    McpProfileMigrationError::Legacy("旧 MCP 秒级超时字段无效".to_string())
                })?;
            return Ok(Some((seconds * 1000.0).round() as u64));
        }
    }
    Ok(None)
}

fn string_value(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn string_map(value: Option<&Value>) -> Result<BTreeMap<String, String>, McpProfileMigrationError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        McpProfileMigrationError::Legacy("MCP 字符串映射字段类型错误".to_string())
    })?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| McpProfileMigrationError::Legacy("MCP 映射值不是字符串".to_string()))
        })
        .collect()
}

fn string_list(value: Option<&Value>) -> Result<Vec<String>, McpProfileMigrationError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| McpProfileMigrationError::Legacy("MCP 列表字段类型错误".to_string()))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| McpProfileMigrationError::Legacy("MCP 列表值不是字符串".to_string()))
        })
        .collect()
}

fn optional_string_list(
    value: Option<&Value>,
) -> Result<Option<Vec<String>>, McpProfileMigrationError> {
    value.map(|_| string_list(value)).transpose()
}

fn publish_marker(path: &Path) -> Result<(), McpProfileMigrationError> {
    let marker = serde_json::to_vec_pretty(&MigrationMarker {
        schema: "muse.mcp-config-migration/v1",
        source: "mcp/servers.json + models/config.json + system-credential-store",
    })
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    atomic_write_synced(path, &marker)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MCP_MIGRATION_MARKER, McpProfileMigrationError, migrate_mcp_profiles};
    use crate::app::preferences::MuseConfigStore;
    use crate::app::secret::{SecretStoreBackend, SecretStoreError};
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct MemorySecrets {
        values: RefCell<BTreeMap<String, String>>,
        fail_delete_account: RefCell<Option<String>>,
        failed_once: Cell<bool>,
    }

    impl MemorySecrets {
        fn with_values(values: BTreeMap<String, String>) -> Self {
            Self {
                values: RefCell::new(values),
                ..Self::default()
            }
        }

        fn fail_delete_once(&self, account: &str) {
            *self.fail_delete_account.borrow_mut() = Some(account.to_string());
        }
    }

    impl SecretStoreBackend for MemorySecrets {
        fn get_optional(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
            Ok(self.values.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
            self.values
                .borrow_mut()
                .insert(account.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
            let should_fail = self
                .fail_delete_account
                .borrow()
                .as_deref()
                .is_some_and(|configured| configured == account)
                && !self.failed_once.replace(true);
            if should_fail {
                return Err(SecretStoreError("模拟凭据清理失败".to_string()));
            }
            self.values.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "muse-mcp-migration-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos()
        ))
    }

    fn write_legacy_store(root: &Path, servers: BTreeMap<String, serde_json::Value>) {
        let path = root.join("mcp/servers.json");
        std::fs::create_dir_all(path.parent().expect("旧 MCP 目录应有父目录"))
            .expect("应创建旧 MCP 目录");
        let content = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "revision": 1,
            "servers": servers,
        }))
        .expect("应序列化旧 MCP 配置");
        std::fs::write(path, content).expect("应写入旧 MCP 配置");
    }

    fn assert_secret_only_in_config(root: &Path, secret: &str) {
        fn visit(path: &Path, files: &mut Vec<PathBuf>) {
            if path.is_file() {
                files.push(path.to_path_buf());
                return;
            }
            for entry in std::fs::read_dir(path).expect("应遍历迁移测试目录") {
                visit(&entry.expect("目录项应可读").path(), files);
            }
        }

        let mut files = Vec::new();
        visit(root, &mut files);
        for path in files {
            let content = std::fs::read(&path).expect("迁移文件应可读");
            if content
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
            {
                assert_eq!(
                    path,
                    root.join("config.toml"),
                    "MCP API Key 不得扩散到 config.toml 之外"
                );
            }
        }
    }

    #[test]
    fn fresh_directory_publishes_empty_mcp_table_once() {
        let root = test_dir("fresh");
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");
        let migrated = migrate_mcp_profiles(
            &root,
            &mut config,
            &BTreeMap::new(),
            &MemorySecrets::default(),
        )
        .expect("全新目录应发布空 MCP 配置");

        assert!(migrated);
        assert!(config.has_published_mcp_profiles());
        assert!(root.join(MCP_MIGRATION_MARKER).is_file());
        let content =
            std::fs::read_to_string(root.join("config.toml")).expect("应能回读 config.toml");
        assert!(content.contains("[mcp_servers]"));
        let repeated = migrate_mcp_profiles(
            &root,
            &mut config,
            &BTreeMap::new(),
            &MemorySecrets::default(),
        )
        .expect("重复启动应直接复用迁移标记");
        assert!(!repeated);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn migrates_json_and_keyring_values_before_cleanup() {
        let root = test_dir("keyring");
        let legacy_servers = BTreeMap::from([
            (
                "github".to_string(),
                json!({
                    "type": "stdio",
                    "command": "npx",
                    "args": ["-y", "github-mcp"],
                    "tool_timeout_sec": 90,
                    "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"},
                    "secret_bindings": {"GITHUB_TOKEN": "mcp.github.token"}
                }),
            ),
            (
                "remote".to_string(),
                json!({
                    "type": "http",
                    "url": "https://example.test/mcp",
                    "bearer_token_env_var": "REMOTE_TOKEN",
                    "secret_bindings": {"REMOTE_TOKEN": "mcp.remote.token"}
                }),
            ),
        ]);
        write_legacy_store(&root, legacy_servers);
        let old_content = std::fs::read(root.join("mcp/servers.json")).expect("应读取旧 MCP JSON");
        let secrets = MemorySecrets::with_values(BTreeMap::from([
            ("mcp.github.token".to_string(), "github-secret".to_string()),
            ("mcp.remote.token".to_string(), "remote-secret".to_string()),
        ]));
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");

        migrate_mcp_profiles(&root, &mut config, &BTreeMap::new(), &secrets)
            .expect("旧 MCP JSON 和凭据应迁移成功");

        let reloaded = MuseConfigStore::load_from_dir(&root).expect("应回读迁移结果");
        assert_eq!(
            reloaded.mcp_profiles().mcp_servers["github"].secret_env["GITHUB_TOKEN"],
            "github-secret"
        );
        assert_eq!(
            reloaded.mcp_profiles().mcp_servers["github"].request_timeout_ms,
            Some(90_000)
        );
        assert_eq!(
            reloaded.mcp_profiles().mcp_servers["remote"].secret_headers["Authorization"],
            "Bearer remote-secret"
        );
        assert!(secrets.values.borrow().is_empty());
        assert_eq!(
            std::fs::read(root.join("mcp/servers.json")).expect("旧 JSON 应继续存在"),
            old_content
        );
        assert_secret_only_in_config(&root, "github-secret");
        assert_secret_only_in_config(&root, "remote-secret");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn migrates_legacy_model_field_and_environment_reference() {
        let root = test_dir("legacy-model-env");
        let environment_name = format!(
            "MUSE_MCP_MIGRATION_TEST_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos()
        );
        unsafe {
            std::env::set_var(&environment_name, "environment-secret");
        }
        let legacy_model_servers = BTreeMap::from([(
            "legacy".to_string(),
            json!({
                "type": "stdio",
                "command": "legacy-mcp",
                "env": {"TOKEN": format!("${{{environment_name}}}")}
            }),
        )]);
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");

        let result = migrate_mcp_profiles(
            &root,
            &mut config,
            &legacy_model_servers,
            &MemorySecrets::default(),
        );
        unsafe {
            std::env::remove_var(&environment_name);
        }
        result.expect("旧模型字段和环境引用应迁移成功");
        assert_eq!(
            config.mcp_profiles().mcp_servers["legacy"].secret_env["TOKEN"],
            "environment-secret"
        );
        assert!(!root.join("mcp/servers.json").exists());
        assert_secret_only_in_config(&root, "environment-secret");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_failure_retries_without_requiring_deleted_secrets() {
        let root = test_dir("retry");
        write_legacy_store(
            &root,
            BTreeMap::from([(
                "demo".to_string(),
                json!({
                    "type": "stdio",
                    "command": "demo",
                    "env_vars": ["TOKEN_A", "TOKEN_B"],
                    "secret_bindings": {
                        "TOKEN_A": "account-a",
                        "TOKEN_B": "account-b"
                    }
                }),
            )]),
        );
        let secrets = MemorySecrets::with_values(BTreeMap::from([
            ("account-a".to_string(), "secret-a".to_string()),
            ("account-b".to_string(), "secret-b".to_string()),
        ]));
        secrets.fail_delete_once("account-b");
        let mut config = MuseConfigStore::load_from_dir(&root).expect("应创建默认配置");

        let first = migrate_mcp_profiles(&root, &mut config, &BTreeMap::new(), &secrets)
            .expect_err("首次清理应按模拟条件失败");
        assert!(matches!(first, McpProfileMigrationError::Secret(_)));
        assert!(config.has_published_mcp_profiles());
        assert!(!root.join(MCP_MIGRATION_MARKER).exists());
        assert!(!secrets.values.borrow().contains_key("account-a"));

        migrate_mcp_profiles(&root, &mut config, &BTreeMap::new(), &secrets)
            .expect("重试不应重新读取已经删除的 account-a");
        assert!(secrets.values.borrow().is_empty());
        assert!(root.join(MCP_MIGRATION_MARKER).is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}
