//! 模型目录 DTO、Provider Profile 校验，以及旧 SQLite 目录的一次性迁移读取。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::model::config::ModelSecretBinding;

use crate::model::profile::{
    DEEPSEEK_PROVIDER_PROFILE, DEFAULT_MODEL_CAPABILITY_DEFAULTS,
    VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL, VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE,
};
use crate::model::vendor::provider_support_capabilities;
use crate::storage;

/// 模型能力目录读写错误。
#[derive(Debug)]
pub enum ModelCatalogError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Storage(storage::RuntimeStorageError),
    Validation(String),
    NotFound(String),
    Conflict(String),
    Config(crate::app::preferences::MuseConfigStoreError),
}

impl std::fmt::Display for ModelCatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelCatalogError::Io(err) => write!(f, "模型能力目录文件读写失败：{err}"),
            ModelCatalogError::Sqlite(err) => write!(f, "模型能力目录数据库操作失败：{err}"),
            ModelCatalogError::Storage(err) => write!(f, "模型能力目录初始化失败：{err}"),
            ModelCatalogError::Validation(message)
            | ModelCatalogError::NotFound(message)
            | ModelCatalogError::Conflict(message) => f.write_str(message),
            ModelCatalogError::Config(error) => write!(f, "模型配置发布失败：{error}"),
        }
    }
}

impl std::error::Error for ModelCatalogError {}

impl From<std::io::Error> for ModelCatalogError {
    fn from(value: std::io::Error) -> Self {
        ModelCatalogError::Io(value)
    }
}

impl From<rusqlite::Error> for ModelCatalogError {
    fn from(value: rusqlite::Error) -> Self {
        ModelCatalogError::Sqlite(value)
    }
}

impl From<storage::RuntimeStorageError> for ModelCatalogError {
    fn from(value: storage::RuntimeStorageError) -> Self {
        ModelCatalogError::Storage(value)
    }
}

/// 前端配置页使用的模型目录快照。
#[derive(Debug, Clone, Serialize)]
pub struct ModelCatalog {
    pub providers: Vec<ModelProviderCatalog>,
    pub models: Vec<ModelCatalogItem>,
    pub capabilities: Vec<String>,
}

/// 凭据引用异常的稳定诊断，字段路径和中文消息不包含任何秘密。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelCredentialDiagnostic {
    pub code: String,
    pub field_path: String,
    pub message: String,
}

/// 前端展示用的模型提供器目录条目。
#[derive(Debug, Clone, Serialize)]
pub struct ModelProviderCatalog {
    pub id: String,
    pub name: String,
    /// 接口返回沿用旧字段名，避免前端配置结构跟着数据库列名频繁迁移。
    pub default_api_base: String,
    pub chat_model_list_url: String,
    pub tts_model_list_url: String,
    pub enabled: bool,
    pub notes: String,
    /// 是否支持供应商专属余额检测。
    pub supports_balance_check: bool,
    /// 当前产品允许使用的能力类型。
    pub capabilities: Vec<String>,
    /// 拉取模型列表时的鉴权要求。
    pub model_list_auth: String,
    /// 是否允许用户编辑 API Base。
    pub allow_custom_base: bool,
    /// 连接验证方式。
    pub connection_validation: String,
    /// `supported` 或 `legacy_unsupported`。
    pub status: String,
    /// 该供应商是否已经在受保护的 `config.toml` 中配置密钥。
    pub api_key_configured: bool,
    /// 兼容旧客户端的可选诊断；Provider Profile 运行时不再产生凭据引用诊断。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_diagnostic: Option<ModelCredentialDiagnostic>,
    /// Provider Profile 的内部密钥副本；目录接口不向前端泄露明文。
    #[serde(skip_serializing)]
    pub api_key: String,
    /// 旧 SQLite 迁移读取字段，不进入目录响应。
    #[serde(skip_serializing)]
    pub credential_account: String,
    /// 旧 SQLite 迁移读取字段，不进入目录响应。
    #[serde(skip_serializing)]
    pub credential_identity: String,
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
    /// 展示标签，例如 tool、reasoning、image_understanding。
    pub tags: Vec<String>,
    /// 功能路由，例如 chat、tts。
    pub functions: Vec<String>,
    /// 前端展示用的合并标签：functions + tags。
    pub capabilities: Vec<String>,
    /// 模型上下文窗口大小。
    pub context_window: u64,
    /// 默认最大输出 token 预留。
    pub default_max_output_tokens: u32,
    /// 是否声明支持返回 usage。
    pub supports_usage: bool,
    /// 是否声明支持缓存 token 明细。
    pub supports_cached_tokens: bool,
    /// 是否声明支持 reasoning token 明细。
    pub supports_reasoning_tokens: bool,
    /// 本地估算使用的 tokenizer 家族标识。
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

/// 旧版基于 SQLite 的模型能力目录，仅供 Provider Profile 首次迁移读取。
///
/// 旧目录包含两张业务表：
/// - providers：提供器名称、基础地址、密钥、聊天模型列表地址、语音模型列表地址。
/// - models：提供器下的模型标识、名称、展示标签和功能路由。
///
/// 当前运行时不再调用该 Store；迁移成功后统一 schema migration 会删除这些表。
#[derive(Debug, Clone)]
pub struct LegacyModelCatalogStore {
    db_path: PathBuf,
}

impl LegacyModelCatalogStore {
    /// 加载旧模型能力目录。这里故意不走统一 runtime migration；调用方必须先读取
    /// 旧表并发布 TOML，随后才能触发删除旧表的 schema migration。
    pub fn load_from_dir(base_dir: impl AsRef<Path>) -> Result<Self, ModelCatalogError> {
        let db_path = Self::db_path(base_dir.as_ref());
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        init_schema(&mut conn)?;
        seed_builtin_providers(&mut conn)?;
        seed_builtin_models(&mut conn)?;
        Ok(Self { db_path })
    }

    /// 读取目录快照，供 `/api/models/catalog` 返回。
    pub fn catalog(&self) -> Result<ModelCatalog, ModelCatalogError> {
        let conn = open_legacy_catalog_connection(&self.db_path)?;
        let providers = load_providers(&conn)?;
        let models = load_models(&conn)?;
        let capabilities = collect_capabilities(&models);
        Ok(ModelCatalog {
            providers,
            models,
            capabilities,
        })
    }

    /// 读取单个供应商事实，包含仅供后端使用的凭据引用。
    pub fn provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ModelProviderCatalog>, ModelCatalogError> {
        let conn = open_legacy_catalog_connection(&self.db_path)?;
        load_provider(&conn, provider_id).map_err(Into::into)
    }

    /// 创建模型。已禁用的同标识模型会被恢复，活动模型不会被静默覆盖。
    pub fn create_model(
        &self,
        draft: ModelCatalogModelDraft,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        let draft = validate_model_draft(draft)?;
        let mut conn = open_legacy_catalog_connection(&self.db_path)?;
        let tx = conn.transaction()?;
        require_enabled_provider(&tx, &draft.provider_id)?;
        let existing_enabled = tx
            .query_row(
                "SELECT enabled FROM models WHERE provider_id = ?1 AND model_id = ?2",
                params![draft.provider_id, draft.model],
                |row| row.get::<_, i32>(0),
            )
            .optional()?;
        if existing_enabled.is_some_and(|enabled| enabled != 0) {
            return Err(ModelCatalogError::Conflict(format!(
                "模型 `{}` 已存在，请直接编辑该模型。",
                draft.model
            )));
        }
        upsert_model(&tx, &draft)?;
        tx.commit()?;
        self.model(&draft.provider_id, &draft.model)?
            .ok_or_else(|| ModelCatalogError::NotFound("模型创建后无法读取。".to_string()))
    }

    /// 编辑模型展示和能力字段；供应商与模型 ID 保持不可变。
    pub fn update_model(
        &self,
        draft: ModelCatalogModelDraft,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        let draft = validate_model_draft(draft)?;
        let mut conn = open_legacy_catalog_connection(&self.db_path)?;
        let tx = conn.transaction()?;
        require_enabled_provider(&tx, &draft.provider_id)?;
        let changed = tx.execute(
            "UPDATE models SET
                model_name = ?3,
                model_tags = ?4,
                model_functions = ?5,
                notes = ?6,
                context_window = ?7,
                default_max_output_tokens = ?8,
                supports_usage = ?9,
                supports_cached_tokens = ?10,
                supports_reasoning_tokens = ?11,
                tokenizer_family = ?12
             WHERE provider_id = ?1 AND model_id = ?2 AND enabled = 1",
            params![
                draft.provider_id,
                draft.model,
                draft.name,
                join_csv(&draft.tags),
                join_csv(&draft.functions),
                draft.notes,
                draft.context_window as i64,
                draft.default_max_output_tokens as i64,
                draft.supports_usage as i32,
                draft.supports_cached_tokens as i32,
                draft.supports_reasoning_tokens as i32,
                draft.tokenizer_family,
            ],
        )?;
        if changed == 0 {
            return Err(ModelCatalogError::NotFound(format!(
                "模型 `{}` 不存在或已删除。",
                draft.model
            )));
        }
        tx.commit()?;
        self.model(&draft.provider_id, &draft.model)?
            .ok_or_else(|| ModelCatalogError::NotFound("模型编辑后无法读取。".to_string()))
    }

    /// 以禁用墓碑删除模型，避免内置模型在下一次启动时被种子数据复活。
    pub fn disable_model(&self, provider_id: &str, model: &str) -> Result<(), ModelCatalogError> {
        let provider_id = validate_identifier("供应商", provider_id, 128)?;
        let model = validate_identifier("模型 ID", model, 256)?;
        let conn = open_legacy_catalog_connection(&self.db_path)?;
        let changed = conn.execute(
            "UPDATE models SET enabled = 0 WHERE provider_id = ?1 AND model_id = ?2 AND enabled = 1",
            params![provider_id, model],
        )?;
        if changed == 0 {
            return Err(ModelCatalogError::NotFound(format!(
                "模型 `{model}` 不存在或已删除。"
            )));
        }
        Ok(())
    }

    /// 确保当前运行模型进入目录，修复手工模型已生效但选择器仍显示旧版本的问题。
    pub fn ensure_runtime_model(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Result<ModelCatalogItem, ModelCatalogError> {
        let provider_id = validate_identifier("供应商", provider_id, 128)?;
        let model = validate_identifier("模型 ID", model, 256)?;
        if let Some(existing) = self.model(&provider_id, &model)? {
            return Ok(existing);
        }
        let defaults = crate::model::profile::provider_profile_for_identity(&provider_id, "")
            .map(|profile| profile.model_defaults)
            .unwrap_or(DEFAULT_MODEL_CAPABILITY_DEFAULTS);
        self.create_model(ModelCatalogModelDraft {
            provider_id,
            model: model.clone(),
            name: model,
            notes: "从当前运行配置自动补录，可在模型配置页完善名称与能力。".to_string(),
            tags: vec!["reasoning".to_string(), "tool".to_string()],
            functions: default_chat_functions(),
            context_window: defaults.context_window,
            default_max_output_tokens: defaults.default_max_output_tokens,
            supports_usage: true,
            supports_cached_tokens: false,
            supports_reasoning_tokens: true,
            tokenizer_family: defaults.tokenizer_family.to_string(),
        })
    }

    /// 读取单个启用模型。
    pub fn model(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Result<Option<ModelCatalogItem>, ModelCatalogError> {
        let conn = open_legacy_catalog_connection(&self.db_path)?;
        load_model(&conn, provider_id, model).map_err(Into::into)
    }

    /// 绑定供应商级系统凭据引用；不会把密钥写入 SQLite。
    pub fn set_provider_credential_binding(
        &self,
        provider_id: &str,
        binding: Option<&ModelSecretBinding>,
    ) -> Result<(), ModelCatalogError> {
        let provider_id = validate_identifier("供应商", provider_id, 128)?;
        let conn = open_legacy_catalog_connection(&self.db_path)?;
        let (account, identity) = binding
            .map(|binding| (binding.account.as_str(), binding.identity.as_str()))
            .unwrap_or(("", ""));
        let changed = conn.execute(
            "UPDATE providers SET credential_account = ?2, credential_identity = ?3 WHERE id = ?1 AND enabled = 1",
            params![provider_id, account, identity],
        )?;
        if changed == 0 {
            return Err(ModelCatalogError::NotFound(
                "当前供应商不存在或已停用。".to_string(),
            ));
        }
        Ok(())
    }

    fn db_path(base_dir: &Path) -> PathBuf {
        storage::runtime_database_path(base_dir)
    }
}

fn open_legacy_catalog_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(connection)
}

fn init_schema(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    if table_exists(conn, "providers")? && !table_has_column(conn, "providers", "base_url")? {
        migrate_legacy_schema(conn)?;
    }

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS providers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            base_url TEXT NOT NULL DEFAULT '',
            api_key TEXT NOT NULL DEFAULT '',
            chat_model_list_url TEXT NOT NULL DEFAULT '',
            tts_model_list_url TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            notes TEXT NOT NULL DEFAULT '',
            credential_account TEXT NOT NULL DEFAULT '',
            credential_identity TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS models (
            provider_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            model_name TEXT NOT NULL,
            model_tags TEXT NOT NULL DEFAULT '',
            model_functions TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            notes TEXT NOT NULL DEFAULT '',
            context_window INTEGER NOT NULL DEFAULT 200000,
            default_max_output_tokens INTEGER NOT NULL DEFAULT 2048,
            supports_usage INTEGER NOT NULL DEFAULT 1,
            supports_cached_tokens INTEGER NOT NULL DEFAULT 1,
            supports_reasoning_tokens INTEGER NOT NULL DEFAULT 1,
            tokenizer_family TEXT NOT NULL DEFAULT 'rough_estimate',
            PRIMARY KEY(provider_id, model_id),
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );

        DROP TABLE IF EXISTS model_capabilities;
        DROP TABLE IF EXISTS catalog_meta;
        PRAGMA foreign_keys = ON;
        "#,
    )?;
    ensure_model_context_columns(conn)?;
    ensure_provider_credential_columns(conn)?;
    Ok(())
}

fn migrate_legacy_schema(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    if !table_has_column(conn, "providers", "model_list_url")? {
        conn.execute(
            "ALTER TABLE providers ADD COLUMN model_list_url TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS providers_next;
        DROP TABLE IF EXISTS models_next;

        CREATE TABLE providers_next (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            base_url TEXT NOT NULL DEFAULT '',
            api_key TEXT NOT NULL DEFAULT '',
            chat_model_list_url TEXT NOT NULL DEFAULT '',
            tts_model_list_url TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            notes TEXT NOT NULL DEFAULT '',
            credential_account TEXT NOT NULL DEFAULT '',
            credential_identity TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE models_next (
            provider_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            model_name TEXT NOT NULL,
            model_tags TEXT NOT NULL DEFAULT '',
            model_functions TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            notes TEXT NOT NULL DEFAULT '',
            context_window INTEGER NOT NULL DEFAULT 200000,
            default_max_output_tokens INTEGER NOT NULL DEFAULT 2048,
            supports_usage INTEGER NOT NULL DEFAULT 1,
            supports_cached_tokens INTEGER NOT NULL DEFAULT 1,
            supports_reasoning_tokens INTEGER NOT NULL DEFAULT 1,
            tokenizer_family TEXT NOT NULL DEFAULT 'rough_estimate',
            PRIMARY KEY(provider_id, model_id),
            FOREIGN KEY(provider_id) REFERENCES providers_next(id) ON DELETE CASCADE
        );

        INSERT OR REPLACE INTO providers_next (
            id, name, base_url, api_key, chat_model_list_url, tts_model_list_url, enabled, notes
        )
        SELECT
            id,
            name,
            COALESCE(default_api_base, ''),
            '',
            COALESCE(model_list_url, ''),
            '',
            enabled,
            notes
        FROM providers;

        INSERT OR REPLACE INTO models_next (
            provider_id, model_id, model_name, model_tags, model_functions, enabled, notes,
            context_window, default_max_output_tokens, supports_usage, supports_cached_tokens,
            supports_reasoning_tokens, tokenizer_family
        )
        SELECT
            m.provider_id,
            m.model,
            m.name,
            COALESCE((
                SELECT group_concat(mc.capability, ',')
                FROM model_capabilities mc
                WHERE mc.model_id = m.id
                  AND mc.capability NOT IN ('chat', 'tts')
                  AND mc.capability <> ('a' || 'sr')
            ), ''),
            COALESCE((
                SELECT group_concat(mc.capability, ',')
                FROM model_capabilities mc
                WHERE mc.model_id = m.id
                  AND mc.capability IN ('chat', 'tts')
            ), ''),
            m.enabled,
            m.notes,
            200000,
            2048,
            1,
            1,
            1,
            'rough_estimate'
        FROM models m;

        DROP TABLE IF EXISTS model_capabilities;
        DROP TABLE IF EXISTS catalog_meta;
        DROP TABLE IF EXISTS models;
        DROP TABLE IF EXISTS providers;

        ALTER TABLE providers_next RENAME TO providers;
        ALTER TABLE models_next RENAME TO models;
        "#,
    )
}

fn seed_builtin_providers(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let tx = conn.transaction()?;

    for obsolete_provider in [
        "mock",
        "openai",
        "openai-compatible",
        "anthropic",
        "ollama",
        "openrouter",
        "minimax",
        "kimi",
        "custom",
        "volcengine_ark",
    ] {
        tx.execute(
            "DELETE FROM models WHERE provider_id = ?1",
            params![obsolete_provider],
        )?;
        tx.execute(
            "DELETE FROM providers WHERE id = ?1",
            params![obsolete_provider],
        )?;
    }

    for provider in builtin_providers() {
        tx.execute(
            "INSERT INTO providers (
                id, name, base_url, chat_model_list_url, tts_model_list_url, enabled, notes
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                base_url = excluded.base_url,
                chat_model_list_url = excluded.chat_model_list_url,
                tts_model_list_url = excluded.tts_model_list_url,
                enabled = excluded.enabled,
                notes = excluded.notes",
            params![
                provider.id,
                provider.name,
                provider.base_url,
                provider.chat_model_list_url,
                provider.tts_model_list_url,
                provider.enabled as i32,
                provider.notes
            ],
        )?;
    }

    tx.commit()
}

fn seed_builtin_models(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    for (provider, model, name, notes) in [
        (
            DEEPSEEK_PROVIDER_PROFILE,
            "deepseek-v4-flash",
            "DeepSeek V4 Flash",
            "DeepSeek 当前推荐的快速对话模型。",
        ),
        (
            DEEPSEEK_PROVIDER_PROFILE,
            "deepseek-v4-pro",
            "DeepSeek V4 Pro",
            "DeepSeek 当前推荐的高质量对话模型。",
        ),
        (
            VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE,
            VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL,
            "Doubao Seed 2.0 Pro",
            "火山方舟 Agent Plan 套餐模型；也可新增控制台当前提供的其他套餐模型名。",
        ),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO models (
                provider_id, model_id, model_name, model_tags, model_functions, enabled, notes,
                context_window, default_max_output_tokens, supports_usage, supports_cached_tokens,
                supports_reasoning_tokens, tokenizer_family
             ) VALUES (?1, ?2, ?3, 'reasoning,tool', 'chat', 1, ?4, ?5, ?6, 1, 0, 1, ?7)",
            params![
                provider.id,
                model,
                name,
                notes,
                provider.model_defaults.context_window as i64,
                provider.model_defaults.default_max_output_tokens as i64,
                provider.model_defaults.tokenizer_family,
            ],
        )?;
    }
    Ok(())
}

fn load_providers(conn: &Connection) -> Result<Vec<ModelProviderCatalog>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, name, base_url, api_key, chat_model_list_url, tts_model_list_url, enabled, notes,
                credential_account, credential_identity
         FROM providers
         WHERE enabled = 1
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let default_api_base: String = row.get(2)?;
        let support = provider_support_capabilities(&id, &default_api_base);
        let requires_chat_probe = id == VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE.id;
        let supported = matches!(id.as_str(), "deepseek" | "volcengine_agent_plan");
        let allow_custom_base = false;
        let chat_model_list_url: String = row.get(4)?;
        Ok(ModelProviderCatalog {
            id,
            name: row.get(1)?,
            default_api_base,
            api_key: row.get(3)?,
            chat_model_list_url: chat_model_list_url.clone(),
            tts_model_list_url: row.get(5)?,
            enabled: row.get::<_, i32>(6)? != 0,
            notes: row.get(7)?,
            supports_balance_check: support.supports_balance_check,
            capabilities: vec!["chat".to_string()],
            model_list_auth: if requires_chat_probe {
                "required"
            } else if chat_model_list_url.trim().is_empty() {
                "none"
            } else {
                "required"
            }
            .to_string(),
            allow_custom_base,
            connection_validation: if requires_chat_probe {
                "chat_probe"
            } else if chat_model_list_url.trim().is_empty() {
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
            // SQLite 只保存非敏感引用，不能据此推断系统凭据是否真实存在。
            api_key_configured: false,
            credential_diagnostic: None,
            credential_account: row.get(8)?,
            credential_identity: row.get(9)?,
        })
    })?;
    rows.collect()
}

fn load_models(conn: &Connection) -> Result<Vec<ModelCatalogItem>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT
            models.provider_id,
            models.model_id,
            models.model_name,
            providers.base_url,
            models.enabled,
            models.notes,
            models.model_tags,
            models.model_functions,
            models.context_window,
            models.default_max_output_tokens,
            models.supports_usage,
            models.supports_cached_tokens,
            models.supports_reasoning_tokens,
            models.tokenizer_family
         FROM models
         JOIN providers ON providers.id = models.provider_id
         WHERE providers.enabled = 1 AND models.enabled = 1
         ORDER BY models.provider_id COLLATE NOCASE, models.model_name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| {
        let provider_id: String = row.get(0)?;
        let model_id: String = row.get(1)?;
        let tags = split_csv(&row.get::<_, String>(6)?);
        let functions = split_csv(&row.get::<_, String>(7)?);
        Ok(ModelCatalogItem {
            id: format!("{}:{}", provider_id, model_id),
            provider_id,
            name: row.get(2)?,
            model: model_id,
            default_api_base: row.get(3)?,
            enabled: row.get::<_, i32>(4)? != 0,
            notes: row.get(5)?,
            capabilities: merge_labels(&functions, &tags),
            tags,
            functions,
            context_window: row
                .get::<_, i64>(8)
                .unwrap_or(DEFAULT_MODEL_CAPABILITY_DEFAULTS.context_window as i64)
                .max(1) as u64,
            default_max_output_tokens: row
                .get::<_, i64>(9)
                .unwrap_or(DEFAULT_MODEL_CAPABILITY_DEFAULTS.default_max_output_tokens as i64)
                .max(1) as u32,
            supports_usage: row.get::<_, i32>(10).unwrap_or(1) != 0,
            supports_cached_tokens: row.get::<_, i32>(11).unwrap_or(1) != 0,
            supports_reasoning_tokens: row.get::<_, i32>(12).unwrap_or(1) != 0,
            tokenizer_family: row.get::<_, String>(13).unwrap_or_else(|_| {
                DEFAULT_MODEL_CAPABILITY_DEFAULTS
                    .tokenizer_family
                    .to_string()
            }),
        })
    })?;
    rows.collect()
}

fn load_provider(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<ModelProviderCatalog>, rusqlite::Error> {
    load_providers(conn).map(|providers| providers.into_iter().find(|item| item.id == provider_id))
}

fn load_model(
    conn: &Connection,
    provider_id: &str,
    model: &str,
) -> Result<Option<ModelCatalogItem>, rusqlite::Error> {
    load_models(conn).map(|models| {
        models
            .into_iter()
            .find(|item| item.provider_id == provider_id && item.model == model)
    })
}

fn require_enabled_provider(conn: &Connection, provider_id: &str) -> Result<(), ModelCatalogError> {
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM providers WHERE id = ?1 AND enabled = 1)",
        params![provider_id],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if !exists {
        return Err(ModelCatalogError::Validation(
            "创建模型时只能选择现有且已启用的供应商。".to_string(),
        ));
    }
    Ok(())
}

fn upsert_model(conn: &Connection, draft: &ModelCatalogModelDraft) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO models (
            provider_id, model_id, model_name, model_tags, model_functions, enabled, notes,
            context_window, default_max_output_tokens, supports_usage, supports_cached_tokens,
            supports_reasoning_tokens, tokenizer_family
         ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(provider_id, model_id) DO UPDATE SET
            model_name = excluded.model_name,
            model_tags = excluded.model_tags,
            model_functions = excluded.model_functions,
            enabled = 1,
            notes = excluded.notes,
            context_window = excluded.context_window,
            default_max_output_tokens = excluded.default_max_output_tokens,
            supports_usage = excluded.supports_usage,
            supports_cached_tokens = excluded.supports_cached_tokens,
            supports_reasoning_tokens = excluded.supports_reasoning_tokens,
            tokenizer_family = excluded.tokenizer_family",
        params![
            draft.provider_id,
            draft.model,
            draft.name,
            join_csv(&draft.tags),
            join_csv(&draft.functions),
            draft.notes,
            draft.context_window as i64,
            draft.default_max_output_tokens as i64,
            draft.supports_usage as i32,
            draft.supports_cached_tokens as i32,
            draft.supports_reasoning_tokens as i32,
            draft.tokenizer_family,
        ],
    )?;
    Ok(())
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

fn join_csv(values: &[String]) -> String {
    values.join(",")
}

fn ensure_provider_credential_columns(conn: &Connection) -> Result<(), rusqlite::Error> {
    for (column, sql) in [
        (
            "credential_account",
            "ALTER TABLE providers ADD COLUMN credential_account TEXT NOT NULL DEFAULT ''",
        ),
        (
            "credential_identity",
            "ALTER TABLE providers ADD COLUMN credential_identity TEXT NOT NULL DEFAULT ''",
        ),
    ] {
        if !table_has_column(conn, "providers", column)? {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

fn ensure_model_context_columns(conn: &Connection) -> Result<(), rusqlite::Error> {
    let columns = [
        (
            "context_window",
            "ALTER TABLE models ADD COLUMN context_window INTEGER NOT NULL DEFAULT 200000",
        ),
        (
            "default_max_output_tokens",
            "ALTER TABLE models ADD COLUMN default_max_output_tokens INTEGER NOT NULL DEFAULT 2048",
        ),
        (
            "supports_usage",
            "ALTER TABLE models ADD COLUMN supports_usage INTEGER NOT NULL DEFAULT 1",
        ),
        (
            "supports_cached_tokens",
            "ALTER TABLE models ADD COLUMN supports_cached_tokens INTEGER NOT NULL DEFAULT 1",
        ),
        (
            "supports_reasoning_tokens",
            "ALTER TABLE models ADD COLUMN supports_reasoning_tokens INTEGER NOT NULL DEFAULT 1",
        ),
        (
            "tokenizer_family",
            "ALTER TABLE models ADD COLUMN tokenizer_family TEXT NOT NULL DEFAULT 'rough_estimate'",
        ),
    ];
    for (column, sql) in columns {
        if !table_has_column(conn, "models", column)? {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

fn collect_capabilities(models: &[ModelCatalogItem]) -> Vec<String> {
    let mut values = BTreeSet::new();
    for model in models {
        values.extend(model.capabilities.iter().cloned());
    }
    values.into_iter().collect()
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table],
        |row| row.get::<_, i32>(0),
    )
    .map(|value| value != 0)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for item in rows {
        if item? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn split_csv(value: &str) -> Vec<String> {
    let mut values: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect();
    values.sort();
    values.dedup();
    values
}

pub(crate) fn merge_labels(functions: &[String], tags: &[String]) -> Vec<String> {
    let mut values = BTreeSet::new();
    values.extend(functions.iter().cloned());
    values.extend(tags.iter().cloned());
    values.into_iter().collect()
}

struct BuiltinProvider {
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    chat_model_list_url: &'static str,
    tts_model_list_url: &'static str,
    enabled: bool,
    notes: &'static str,
}

fn builtin_providers() -> Vec<BuiltinProvider> {
    let deepseek = DEEPSEEK_PROVIDER_PROFILE;
    let agent_plan = VOLCENGINE_AGENT_PLAN_PROVIDER_PROFILE;
    vec![
        BuiltinProvider {
            id: deepseek.id,
            name: deepseek.name,
            base_url: deepseek.default_api_base,
            chat_model_list_url: deepseek.chat_model_list_url,
            tts_model_list_url: "",
            enabled: true,
            notes: deepseek.notes,
        },
        BuiltinProvider {
            id: agent_plan.id,
            name: agent_plan.name,
            base_url: agent_plan.default_api_base,
            chat_model_list_url: agent_plan.chat_model_list_url,
            tts_model_list_url: "",
            enabled: true,
            notes: agent_plan.notes,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{LegacyModelCatalogStore, ModelCatalogError, ModelCatalogModelDraft};
    use crate::model::config::ModelSecretBinding;
    use crate::model::profile::VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        std::env::temp_dir().join(format!("muse-model-catalog-{suffix}"))
    }

    #[test]
    fn initializes_catalog_database_with_two_business_tables() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let catalog = store.catalog().expect("读取模型目录");
        assert_eq!(
            catalog
                .providers
                .iter()
                .map(|item| item.id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["deepseek", "volcengine_agent_plan"])
        );

        let conn =
            Connection::open(dir.join("runtime").join("muse.sqlite")).expect("打开统一运行时库");
        let table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('providers', 'models')",
                [],
                |row| row.get(0),
            )
            .expect("统计业务表");
        assert_eq!(table_count, 2);
        let legacy_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('catalog_meta', 'model_capabilities')",
                [],
                |row| row.get(0),
            )
            .expect("统计旧表");
        assert_eq!(legacy_count, 0);
    }

    #[test]
    fn keeps_runtime_config_out_of_catalog_response() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let catalog = store.catalog().expect("读取模型目录");
        assert!(
            catalog
                .providers
                .iter()
                .all(|item| !item.notes.contains("api_key"))
        );
    }

    #[test]
    fn exposes_provider_specific_support_capabilities() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let catalog = store.catalog().expect("读取模型目录");
        let deepseek = catalog
            .providers
            .iter()
            .find(|provider| provider.id == "deepseek")
            .expect("目录应包含 DeepSeek");
        let agent_plan = catalog
            .providers
            .iter()
            .find(|provider| provider.id == "volcengine_agent_plan")
            .expect("目录应包含火山方舟 Agent Plan");

        assert!(deepseek.supports_balance_check);
        assert_eq!(deepseek.capabilities, ["chat"]);
        assert_eq!(deepseek.model_list_auth, "required");
        assert_eq!(deepseek.connection_validation, "model_list");
        assert!(!deepseek.allow_custom_base);
        assert_eq!(deepseek.status, "supported");
        assert!(!agent_plan.supports_balance_check);
        assert_eq!(agent_plan.model_list_auth, "required");
        assert_eq!(agent_plan.connection_validation, "chat_probe");
        assert!(!agent_plan.allow_custom_base);
        assert!(catalog.models.iter().any(|model| {
            model.provider_id == "volcengine_agent_plan"
                && model.model == VOLCENGINE_AGENT_PLAN_DEFAULT_MODEL
        }));
    }

    fn test_model(provider_id: &str, model: &str) -> ModelCatalogModelDraft {
        ModelCatalogModelDraft {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
            name: "测试模型".to_string(),
            notes: "用于目录读写测试。".to_string(),
            tags: vec!["tool".to_string()],
            functions: vec!["chat".to_string()],
            context_window: 128_000,
            default_max_output_tokens: 2_048,
            supports_usage: true,
            supports_cached_tokens: false,
            supports_reasoning_tokens: true,
            tokenizer_family: "rough_estimate".to_string(),
        }
    }

    #[test]
    fn creates_updates_and_tombstones_models_without_seed_resurrection() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let created = store
            .create_model(test_model("volcengine_agent_plan", "glm-5.2"))
            .expect("创建模型");
        assert_eq!(created.model, "glm-5.2");

        let mut edited = test_model("volcengine_agent_plan", "glm-5.2");
        edited.name = "GLM 5.2".to_string();
        let updated = store.update_model(edited).expect("编辑模型");
        assert_eq!(updated.name, "GLM 5.2");

        store
            .disable_model("volcengine_agent_plan", "glm-5.2")
            .expect("删除模型");
        assert!(
            store
                .model("volcengine_agent_plan", "glm-5.2")
                .expect("读取模型")
                .is_none()
        );
        let reloaded = LegacyModelCatalogStore::load_from_dir(&dir).expect("重复启动模型目录");
        assert!(
            reloaded
                .model("volcengine_agent_plan", "glm-5.2")
                .expect("读取重启后的模型")
                .is_none()
        );
    }

    #[test]
    fn rejects_models_for_unknown_providers_and_duplicate_active_ids() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let unknown = store
            .create_model(test_model("missing", "demo"))
            .expect_err("未知供应商必须被拒绝");
        assert!(matches!(unknown, ModelCatalogError::Validation(_)));

        store
            .create_model(test_model("deepseek", "demo"))
            .expect("首次创建模型");
        let duplicate = store
            .create_model(test_model("deepseek", "demo"))
            .expect_err("重复模型必须被拒绝");
        assert!(matches!(duplicate, ModelCatalogError::Conflict(_)));
    }

    #[test]
    fn rejects_commas_in_labels_before_csv_storage() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let mut draft = test_model("deepseek", "invalid-label");
        draft.tags = vec!["reasoning,tool".to_string()];

        let error = store
            .create_model(draft)
            .expect_err("包含逗号的标签会破坏 CSV 边界，必须拒绝");

        assert!(matches!(error, ModelCatalogError::Validation(_)));
        assert!(error.to_string().contains("不能包含逗号"));
    }

    #[test]
    fn ensures_runtime_model_and_never_serializes_credential_reference() {
        let dir = unique_temp_dir();
        let store = LegacyModelCatalogStore::load_from_dir(&dir).expect("初始化模型目录");
        let runtime_model = store
            .ensure_runtime_model("volcengine_agent_plan", "glm-5.2")
            .expect("补录运行模型");
        assert_eq!(runtime_model.model, "glm-5.2");

        store
            .set_provider_credential_binding(
                "volcengine_agent_plan",
                Some(&ModelSecretBinding {
                    account: "model.chat.v2.0123456789abcdef0123456789abcdef".to_string(),
                    identity: "test-identity".to_string(),
                }),
            )
            .expect("保存供应商凭据引用");
        let catalog = store.catalog().expect("读取目录");
        let provider = catalog
            .providers
            .iter()
            .find(|provider| provider.id == "volcengine_agent_plan")
            .expect("读取 Agent Plan 供应商");
        assert!(!provider.api_key_configured);
        assert!(provider.credential_diagnostic.is_none());
        let json = serde_json::to_value(&catalog).expect("序列化目录");
        let provider_json = &json["providers"].as_array().expect("供应商数组")[0];
        assert!(provider_json.get("credential_account").is_none());
        assert!(provider_json.get("credential_identity").is_none());
    }
}
