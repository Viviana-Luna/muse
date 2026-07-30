//! Token 用量、上下文快照与运行时持久化结构。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::log_storage::{
    RuntimeLogStorageError, open_runtime_log_database, open_runtime_log_database_at_path,
    validate_runtime_log_database,
};

/// Token 用量来源。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenUsageSource {
    /// 模型提供器真实返回的 usage。
    ProviderReported,
    /// 本地按文本长度估算的 usage。
    LocalEstimated,
}

impl TokenUsageSource {
    /// 返回前端和数据库复用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::LocalEstimated => "local_estimated",
        }
    }
}

impl std::str::FromStr for TokenUsageSource {
    type Err = std::convert::Infallible;

    /// 从稳定字符串恢复枚举；未知旧值按本地估算兼容。
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "provider_reported" => Ok(Self::ProviderReported),
            _ => Ok(Self::LocalEstimated),
        }
    }
}

/// Provider 原始 usage 的统一结构。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub server_tool_tokens: u64,
    pub total_tokens: u64,
    pub source: TokenUsageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<Value>,
}

impl ProviderTokenUsage {
    /// 构造本地估算 usage。
    pub fn local_estimated(input_tokens: u64, output_tokens: u64) -> Self {
        let total_tokens = input_tokens.saturating_add(output_tokens);
        Self {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            reasoning_tokens: 0,
            server_tool_tokens: 0,
            total_tokens,
            source: TokenUsageSource::LocalEstimated,
            raw_usage: None,
        }
    }

    /// 返回上下文窗口应计入的输入侧 token。
    pub fn context_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }

    /// 合并流式累计 usage，避免后续空值或 0 覆盖先前真实输入/缓存字段。
    pub fn merge_cumulative(&mut self, next: ProviderTokenUsage) {
        let next_reports_total = next
            .raw_usage
            .as_ref()
            .and_then(|usage| usage.get("total_tokens"))
            .and_then(|value| value.as_u64())
            .is_some_and(|value| value > 0);
        if next.input_tokens > 0 {
            self.input_tokens = next.input_tokens;
        }
        if next.output_tokens > 0 {
            self.output_tokens = next.output_tokens;
        }
        if next.cache_creation_input_tokens > 0 {
            self.cache_creation_input_tokens = next.cache_creation_input_tokens;
        }
        if next.cache_read_input_tokens > 0 {
            self.cache_read_input_tokens = next.cache_read_input_tokens;
        }
        if next.reasoning_tokens > 0 {
            self.reasoning_tokens = next.reasoning_tokens;
        }
        if next.server_tool_tokens > 0 {
            self.server_tool_tokens = next.server_tool_tokens;
        }
        if next_reports_total {
            self.total_tokens = next.total_tokens;
        } else {
            self.total_tokens = self
                .input_tokens
                .saturating_add(self.output_tokens)
                .saturating_add(self.cache_creation_input_tokens)
                .saturating_add(self.cache_read_input_tokens)
                .saturating_add(self.server_tool_tokens);
        }
        self.source = next.source;
        if next.raw_usage.is_some() {
            self.raw_usage = next.raw_usage;
        }
    }
}

/// 单次模型调用的运行时 Token 用量。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTokenUsage {
    pub id: String,
    pub conversation_id: String,
    pub turn_id: String,
    pub provider: String,
    pub model: String,
    pub created_at: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub server_tool_tokens: u64,
    pub total_tokens: u64,
    pub source: TokenUsageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<Value>,
}

impl RuntimeTokenUsage {
    /// 从 provider usage 和运行时上下文构造可落库记录。
    pub fn from_provider_usage(
        id: impl Into<String>,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        created_at: impl Into<String>,
        usage: ProviderTokenUsage,
    ) -> Self {
        Self {
            id: id.into(),
            conversation_id: conversation_id.into(),
            turn_id: turn_id.into(),
            provider: provider.into(),
            model: model.into(),
            created_at: created_at.into(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            server_tool_tokens: usage.server_tool_tokens,
            total_tokens: usage.total_tokens,
            source: usage.source,
            raw_usage: usage.raw_usage,
        }
    }
}

/// 上下文窗口中的单个来源片段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeContextSegment {
    pub kind: String,
    pub label: String,
    pub tokens: u64,
    pub source: TokenUsageSource,
    #[serde(default)]
    pub compacted: bool,
    #[serde(default)]
    pub externalized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// 单轮模型调用前后的上下文窗口快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeContextSnapshot {
    pub conversation_id: String,
    pub turn_id: String,
    pub provider: String,
    pub model: String,
    pub created_at: String,
    pub context_window: u64,
    pub reserved_output_tokens: u64,
    pub used_input_tokens: u64,
    pub used_cache_tokens: u64,
    pub used_total_tokens: u64,
    pub remaining_tokens: u64,
    pub usage_percent: f64,
    pub source: TokenUsageSource,
    pub compacted: bool,
    pub externalized_tool_results: bool,
    pub segments: Vec<RuntimeContextSegment>,
}

impl RuntimeContextSnapshot {
    /// 用真实 provider usage 回填输入侧统计。
    pub fn with_provider_usage(mut self, usage: &ProviderTokenUsage) -> Self {
        if usage.source != TokenUsageSource::ProviderReported {
            return self;
        }
        let used_input_tokens = usage.input_tokens;
        let used_cache_tokens = usage
            .cache_creation_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        let used_total_tokens = used_input_tokens.saturating_add(used_cache_tokens);
        self.used_input_tokens = used_input_tokens;
        self.used_cache_tokens = used_cache_tokens;
        self.used_total_tokens = used_total_tokens;
        self.remaining_tokens = self
            .context_window
            .saturating_sub(self.reserved_output_tokens)
            .saturating_sub(used_total_tokens);
        self.usage_percent = if self.context_window == 0 {
            0.0
        } else {
            (used_total_tokens as f64 / self.context_window as f64) * 100.0
        };
        self.source = TokenUsageSource::ProviderReported;
        self
    }
}

/// 运行时 usage 存储错误。
#[derive(Debug)]
pub enum RuntimeUsageStoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Storage(RuntimeLogStorageError),
    Json(serde_json::Error),
}

impl std::fmt::Display for RuntimeUsageStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeUsageStoreError::Io(err) => write!(f, "运行时用量存储读写失败：{err}"),
            RuntimeUsageStoreError::Sqlite(err) => write!(f, "运行时用量数据库操作失败：{err}"),
            RuntimeUsageStoreError::Storage(err) => write!(f, "运行时用量存储初始化失败：{err}"),
            RuntimeUsageStoreError::Json(err) => write!(f, "运行时用量 JSON 序列化失败：{err}"),
        }
    }
}

impl std::error::Error for RuntimeUsageStoreError {}

impl From<std::io::Error> for RuntimeUsageStoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<rusqlite::Error> for RuntimeUsageStoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<RuntimeLogStorageError> for RuntimeUsageStoreError {
    fn from(value: RuntimeLogStorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<serde_json::Error> for RuntimeUsageStoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// 基于独立日志 SQLite 库的 Token 与上下文快照存储。
#[derive(Debug, Clone)]
pub struct RuntimeUsageStore {
    db_path: PathBuf,
}

/// 对已存在运行时数据库执行只读取证，不创建目录、数据库或 schema。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHistoryEvidence {
    pub has_history: bool,
    pub unknown_tables: Vec<String>,
}

pub fn inspect_runtime_history_evidence_read_only(
    base_dir: impl AsRef<Path>,
) -> Result<RuntimeHistoryEvidence, RuntimeUsageStoreError> {
    let base_dir = base_dir.as_ref();
    let mut evidence = RuntimeHistoryEvidence {
        has_history: false,
        unknown_tables: Vec::new(),
    };
    inspect_database_directory(
        &base_dir.join("runtime"),
        &["muse.sqlite", "agent-vp.sqlite"],
        true,
        &mut evidence,
    )?;
    inspect_database_directory(
        &base_dir.join("logs"),
        &["runtime-usage.sqlite"],
        false,
        &mut evidence,
    )?;
    evidence.unknown_tables.sort();
    evidence.unknown_tables.dedup();
    Ok(evidence)
}

fn inspect_database_directory(
    directory: &Path,
    database_names: &[&str],
    core_database: bool,
    evidence: &mut RuntimeHistoryEvidence,
) -> Result<(), RuntimeUsageStoreError> {
    match std::fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(std::io::Error::other(format!(
                "运行数据路径 `{}` 不是普通目录",
                directory.display()
            ))
            .into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with("-journal") {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(std::io::Error::other(format!(
                    "运行数据库 sidecar `{name}` 不能是符号链接"
                ))
                .into());
            }
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::Error::other(format!(
                    "运行数据库 sidecar `{name}` 不是普通文件"
                ))
                .into());
            }
        }
    }
    for database_name in database_names {
        let path = directory.join(database_name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(std::io::Error::other(format!(
                    "运行数据库 `{}` 不是普通文件",
                    path.display()
                ))
                .into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        inspect_history_database(&path, database_name, core_database, evidence)?;
    }
    Ok(())
}

fn inspect_history_database(
    path: &Path,
    database_name: &str,
    core_database: bool,
    evidence: &mut RuntimeHistoryEvidence,
) -> Result<(), RuntimeUsageStoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let quick_check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(std::io::Error::other(format!(
            "运行数据库 `{}` 完整性检查失败：{quick_check}",
            path.display()
        ))
        .into());
    }
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for table in &tables {
        let known = matches!(
            table.as_str(),
            "runtime_token_usage" | "runtime_context_snapshots"
        ) || (!core_database && table == "log_schema_migrations")
            || (core_database
                && matches!(
                    table.as_str(),
                    "providers"
                        | "models"
                        | "schema_migrations"
                        | "session_index"
                        | "session_index_state"
                        | "persona_workspace_state"
                        | "persona_state_event"
                        | "persona_state_projection"
                        | "memory_authority_anchor"
                        | "memory_entry"
                        | "memory_revision"
                        | "memory_revision_source"
                        | "memory_committed_batch"
                        | "memory_committed_operation"
                        | "memory_management_operation"
                        | "memory_search_projection"
                        | "memory_fts"
                        | "memory_fts_data"
                        | "memory_fts_idx"
                        | "memory_fts_docsize"
                        | "memory_fts_config"
                ))
            || table.starts_with("sqlite_");
        if !known {
            evidence
                .unknown_tables
                .push(format!("{database_name}:{table}"));
        }
    }
    for table in ["runtime_token_usage", "runtime_context_snapshots"] {
        if tables.iter().any(|name| name == table) {
            let exists: bool = connection.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} LIMIT 1)"),
                [],
                |row| row.get(0),
            )?;
            evidence.has_history |= exists;
        }
    }
    Ok(())
}

impl RuntimeUsageStore {
    /// 加载或初始化运行时用量存储。
    pub fn load_from_dir(base_dir: impl AsRef<Path>) -> Result<Self, RuntimeUsageStoreError> {
        let (db_path, conn) = open_runtime_log_database(base_dir)?;
        init_schema(&conn)?;
        validate_runtime_log_database(&conn)?;
        Ok(Self { db_path })
    }

    /// 返回独立日志数据库路径，便于诊断和生命周期管理。
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    /// 写入单次模型调用 usage。
    pub fn record_token_usage(
        &self,
        usage: &RuntimeTokenUsage,
    ) -> Result<(), RuntimeUsageStoreError> {
        let conn = self.open_connection()?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO runtime_token_usage (
                id, conversation_id, turn_id, provider, model, created_at,
                input_tokens, output_tokens, cache_creation_input_tokens,
                cache_read_input_tokens, reasoning_tokens, server_tool_tokens,
                total_tokens, source, raw_usage
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
            params![
                usage.id,
                usage.conversation_id,
                usage.turn_id,
                usage.provider,
                usage.model,
                usage.created_at,
                usage.input_tokens as i64,
                usage.output_tokens as i64,
                usage.cache_creation_input_tokens as i64,
                usage.cache_read_input_tokens as i64,
                usage.reasoning_tokens as i64,
                usage.server_tool_tokens as i64,
                usage.total_tokens as i64,
                usage.source.as_str(),
                sanitized_raw_usage(usage.raw_usage.as_ref())
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            ],
        )?;
        Ok(())
    }

    /// 写入上下文快照，按 turn_id 保留最新版本。
    pub fn record_context_snapshot(
        &self,
        snapshot: &RuntimeContextSnapshot,
    ) -> Result<(), RuntimeUsageStoreError> {
        let conn = self.open_connection()?;
        let snapshot_json = serde_json::to_string(&sanitized_context_snapshot(snapshot))?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO runtime_context_snapshots (
                turn_id, conversation_id, provider, model, created_at, snapshot_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                snapshot.turn_id,
                snapshot.conversation_id,
                snapshot.provider,
                snapshot.model,
                snapshot.created_at,
                snapshot_json,
            ],
        )?;
        Ok(())
    }

    /// 读取指定会话的 usage 明细；`since` 为 RFC3339 字符串时按创建时间过滤。
    pub fn list_token_usage(
        &self,
        conversation_id: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<RuntimeTokenUsage>, RuntimeUsageStoreError> {
        let conn = self.open_connection()?;
        let mut rows = match (conversation_id, since) {
            (Some(conversation_id), Some(since)) => {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT id, conversation_id, turn_id, provider, model, created_at,
                           input_tokens, output_tokens, cache_creation_input_tokens,
                           cache_read_input_tokens, reasoning_tokens, server_tool_tokens,
                           total_tokens, source, raw_usage
                    FROM runtime_token_usage
                    WHERE conversation_id = ?1 AND created_at >= ?2
                    ORDER BY created_at ASC
                    "#,
                )?;
                collect_usage_rows(stmt.query(params![conversation_id, since])?)?
            }
            (Some(conversation_id), None) => {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT id, conversation_id, turn_id, provider, model, created_at,
                           input_tokens, output_tokens, cache_creation_input_tokens,
                           cache_read_input_tokens, reasoning_tokens, server_tool_tokens,
                           total_tokens, source, raw_usage
                    FROM runtime_token_usage
                    WHERE conversation_id = ?1
                    ORDER BY created_at ASC
                    "#,
                )?;
                collect_usage_rows(stmt.query(params![conversation_id])?)?
            }
            (None, Some(since)) => {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT id, conversation_id, turn_id, provider, model, created_at,
                           input_tokens, output_tokens, cache_creation_input_tokens,
                           cache_read_input_tokens, reasoning_tokens, server_tool_tokens,
                           total_tokens, source, raw_usage
                    FROM runtime_token_usage
                    WHERE created_at >= ?1
                    ORDER BY created_at ASC
                    "#,
                )?;
                collect_usage_rows(stmt.query(params![since])?)?
            }
            (None, None) => {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT id, conversation_id, turn_id, provider, model, created_at,
                           input_tokens, output_tokens, cache_creation_input_tokens,
                           cache_read_input_tokens, reasoning_tokens, server_tool_tokens,
                           total_tokens, source, raw_usage
                    FROM runtime_token_usage
                    ORDER BY created_at ASC
                    "#,
                )?;
                collect_usage_rows(stmt.query([])?)?
            }
        };
        rows.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(rows)
    }

    /// 读取指定会话最近一次上下文快照。
    pub fn latest_context_snapshot(
        &self,
        conversation_id: &str,
    ) -> Result<Option<RuntimeContextSnapshot>, RuntimeUsageStoreError> {
        let conn = self.open_connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT snapshot_json
            FROM runtime_context_snapshots
            WHERE conversation_id = ?1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )?;
        let mut rows = stmt.query(params![conversation_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let text: String = row.get(0)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// 清理可丢弃的用量与上下文日志，不接触核心 muse.sqlite。
    pub fn clear(&self) -> Result<(), RuntimeUsageStoreError> {
        let mut conn = self.open_connection()?;
        let transaction = conn.transaction()?;
        transaction.execute("DELETE FROM runtime_token_usage", [])?;
        transaction.execute("DELETE FROM runtime_context_snapshots", [])?;
        transaction.commit()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    fn open_connection(&self) -> Result<Connection, RuntimeUsageStoreError> {
        let connection = open_runtime_log_database_at_path(&self.db_path)?;
        init_schema(&connection)?;
        Ok(connection)
    }
}

fn init_schema(conn: &Connection) -> Result<(), RuntimeUsageStoreError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS log_schema_migrations (
            version INTEGER PRIMARY KEY CHECK (version > 0),
            name TEXT NOT NULL UNIQUE,
            applied_at TEXT NOT NULL
        );
        "#,
    )?;
    let columns = conn
        .prepare("PRAGMA table_info(log_schema_migrations)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns != ["version", "name", "applied_at"] {
        return Err(RuntimeLogStorageError::Integrity(
            "log_schema_migrations 表结构无效".to_string(),
        )
        .into());
    }
    let newest: Option<i64> = conn.query_row(
        "SELECT MAX(version) FROM log_schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if let Some(version) = newest
        && version > 1
    {
        return Err(RuntimeLogStorageError::Integrity(format!(
            "日志 schema 版本 {version} 高于当前程序支持的 1"
        ))
        .into());
    }
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS runtime_token_usage (
            id TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_input_tokens INTEGER NOT NULL DEFAULT 0,
            reasoning_tokens INTEGER NOT NULL DEFAULT 0,
            server_tool_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL,
            raw_usage TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_runtime_token_usage_conversation_time
            ON runtime_token_usage(conversation_id, created_at);

        CREATE TABLE IF NOT EXISTS runtime_context_snapshots (
            turn_id TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL,
            snapshot_json TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_runtime_context_snapshots_conversation_time
            ON runtime_context_snapshots(conversation_id, created_at);

        INSERT OR IGNORE INTO log_schema_migrations(version, name, applied_at)
        VALUES (1, 'runtime_usage_log_foundation', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
        "#,
    )?;
    Ok(())
}

fn sanitized_raw_usage(raw_usage: Option<&Value>) -> Option<Value> {
    const SAFE_KEYS: &[&str] = &[
        "usage",
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "prompt_cache_hit_tokens",
        "prompt_cache_miss_tokens",
        "cached_tokens",
        "cache_tokens",
        "reasoning_tokens",
        "server_tool_tokens",
        "input_tokens_details",
        "output_tokens_details",
        "prompt_tokens_details",
        "completion_tokens_details",
    ];

    fn sanitize(value: &Value, safe_keys: &[&str]) -> Option<Value> {
        let object = value.as_object()?;
        let sanitized = object
            .iter()
            .filter(|(key, _)| safe_keys.contains(&key.as_str()))
            .filter_map(|(key, value)| {
                let value = if value.is_number() {
                    Some(value.clone())
                } else {
                    sanitize(value, safe_keys)
                }?;
                Some((key.clone(), value))
            })
            .collect::<serde_json::Map<_, _>>();
        (!sanitized.is_empty()).then_some(Value::Object(sanitized))
    }

    raw_usage.and_then(|value| sanitize(value, SAFE_KEYS))
}

fn sanitized_context_snapshot(snapshot: &RuntimeContextSnapshot) -> RuntimeContextSnapshot {
    let mut sanitized = snapshot.clone();
    for segment in &mut sanitized.segments {
        segment.metadata = None;
    }
    sanitized
}

fn collect_usage_rows(
    mut rows: rusqlite::Rows<'_>,
) -> Result<Vec<RuntimeTokenUsage>, RuntimeUsageStoreError> {
    let mut usages = Vec::new();
    while let Some(row) = rows.next()? {
        let raw_usage_text: Option<String> = row.get(14)?;
        usages.push(RuntimeTokenUsage {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            turn_id: row.get(2)?,
            provider: row.get(3)?,
            model: row.get(4)?,
            created_at: row.get(5)?,
            input_tokens: row.get::<_, i64>(6)? as u64,
            output_tokens: row.get::<_, i64>(7)? as u64,
            cache_creation_input_tokens: row.get::<_, i64>(8)? as u64,
            cache_read_input_tokens: row.get::<_, i64>(9)? as u64,
            reasoning_tokens: row.get::<_, i64>(10)? as u64,
            server_tool_tokens: row.get::<_, i64>(11)? as u64,
            total_tokens: row.get::<_, i64>(12)? as u64,
            source: row
                .get::<_, String>(13)?
                .parse::<TokenUsageSource>()
                .unwrap_or(TokenUsageSource::LocalEstimated),
            raw_usage: raw_usage_text
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        });
    }
    Ok(usages)
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderTokenUsage, RuntimeContextSegment, RuntimeContextSnapshot, RuntimeTokenUsage,
        RuntimeUsageStore, TokenUsageSource, inspect_runtime_history_evidence_read_only,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn unique_temp_dir() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "muse-runtime-usage-{suffix}-{}",
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn sample_usage(raw_usage: Option<serde_json::Value>) -> RuntimeTokenUsage {
        RuntimeTokenUsage::from_provider_usage(
            "usage-1",
            "conversation-1",
            "turn-1",
            "openai",
            "gpt-test",
            "2026-07-05T00:00:00Z",
            ProviderTokenUsage {
                input_tokens: 100,
                output_tokens: 30,
                cache_creation_input_tokens: 10,
                cache_read_input_tokens: 5,
                reasoning_tokens: 7,
                server_tool_tokens: 0,
                total_tokens: 152,
                source: TokenUsageSource::ProviderReported,
                raw_usage,
            },
        )
    }

    fn sample_snapshot(metadata: Option<serde_json::Value>) -> RuntimeContextSnapshot {
        RuntimeContextSnapshot {
            conversation_id: "conversation-1".to_string(),
            turn_id: "turn-1".to_string(),
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            created_at: "2026-07-05T00:00:00Z".to_string(),
            context_window: 200_000,
            reserved_output_tokens: 2048,
            used_input_tokens: 115,
            used_cache_tokens: 0,
            used_total_tokens: 115,
            remaining_tokens: 197_837,
            usage_percent: 0.0575,
            source: TokenUsageSource::LocalEstimated,
            compacted: false,
            externalized_tool_results: false,
            segments: vec![RuntimeContextSegment {
                kind: "system".to_string(),
                label: "系统提示".to_string(),
                tokens: 20,
                source: TokenUsageSource::LocalEstimated,
                compacted: false,
                externalized: false,
                metadata,
            }],
        }
    }

    #[test]
    fn merges_streaming_cumulative_usage_without_zero_overwrite() {
        let mut usage = ProviderTokenUsage {
            input_tokens: 120,
            output_tokens: 0,
            cache_creation_input_tokens: 40,
            cache_read_input_tokens: 20,
            reasoning_tokens: 0,
            server_tool_tokens: 0,
            total_tokens: 180,
            source: TokenUsageSource::ProviderReported,
            raw_usage: None,
        };

        usage.merge_cumulative(ProviderTokenUsage {
            input_tokens: 0,
            output_tokens: 25,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            reasoning_tokens: 5,
            server_tool_tokens: 0,
            total_tokens: 0,
            source: TokenUsageSource::ProviderReported,
            raw_usage: None,
        });

        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cache_creation_input_tokens, 40);
        assert_eq!(usage.cache_read_input_tokens, 20);
        assert_eq!(usage.output_tokens, 25);
        assert_eq!(usage.reasoning_tokens, 5);
        assert_eq!(usage.total_tokens, 205);
    }

    #[test]
    fn streaming_partial_usage_does_not_replace_total_with_output_only_delta() {
        let mut usage = ProviderTokenUsage {
            input_tokens: 100,
            output_tokens: 0,
            cache_creation_input_tokens: 40,
            cache_read_input_tokens: 20,
            reasoning_tokens: 0,
            server_tool_tokens: 0,
            total_tokens: 160,
            source: TokenUsageSource::ProviderReported,
            raw_usage: Some(serde_json::json!({
                "input_tokens": 100,
                "cache_creation_input_tokens": 40,
                "cache_read_input_tokens": 20
            })),
        };

        usage.merge_cumulative(ProviderTokenUsage {
            input_tokens: 0,
            output_tokens: 30,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            reasoning_tokens: 0,
            server_tool_tokens: 0,
            total_tokens: 30,
            source: TokenUsageSource::ProviderReported,
            raw_usage: Some(serde_json::json!({ "output_tokens": 30 })),
        });

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.cache_creation_input_tokens, 40);
        assert_eq!(usage.cache_read_input_tokens, 20);
        assert_eq!(usage.total_tokens, 190);
    }

    #[test]
    fn persists_usage_and_latest_context_snapshot() {
        let dir = unique_temp_dir();
        let store = RuntimeUsageStore::load_from_dir(&dir).expect("初始化用量库");
        assert_eq!(store.database_path(), dir.join("logs/runtime-usage.sqlite"));
        assert!(!dir.join("runtime/muse.sqlite").exists());
        let usage = sample_usage(None);
        store.record_token_usage(&usage).expect("写入 usage");

        let rows = store
            .list_token_usage(Some("conversation-1"), None)
            .expect("读取 usage");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].total_tokens, 152);

        let snapshot = sample_snapshot(None);
        store.record_context_snapshot(&snapshot).expect("写入快照");
        let latest = store
            .latest_context_snapshot("conversation-1")
            .expect("读取快照")
            .expect("应存在快照");
        assert_eq!(latest.turn_id, "turn-1");
        assert_eq!(latest.segments[0].kind, "system");
        std::fs::remove_dir_all(dir).expect("应清理测试目录");
    }

    #[test]
    fn rejects_future_log_schema_without_opening_core_database() {
        let dir = unique_temp_dir();
        let store = RuntimeUsageStore::load_from_dir(&dir).expect("应初始化日志库");
        let connection = rusqlite::Connection::open(store.database_path()).expect("应打开日志库");
        connection
            .execute(
                "INSERT INTO log_schema_migrations(version, name, applied_at)
                 VALUES (99, 'future', 'now')",
                [],
            )
            .expect("应写入未来日志 schema");
        drop(connection);

        let error = store
            .list_token_usage(None, None)
            .expect_err("未来日志 schema 必须拒绝降级读取");
        assert!(error.to_string().contains("版本 99 高于"));
        assert!(!dir.join("runtime/muse.sqlite").exists());
        std::fs::remove_dir_all(dir).expect("应清理测试目录");
    }

    #[test]
    fn log_cleanup_and_core_backup_keep_lifecycles_isolated() {
        let dir = unique_temp_dir();
        let (_, core) = crate::storage::open_runtime_database(&dir).expect("应创建核心库");
        core.execute(
            "INSERT INTO session_index(
                 conversation_id, event_file, fallback_summary, summary, record_count,
                 metadata_revision, last_commit_seq
             ) VALUES ('core-probe', 'sessions/core.jsonl', '核心状态', '核心状态', 1, 0, 1)",
            [],
        )
        .expect("应写入核心状态");
        drop(core);

        let store = RuntimeUsageStore::load_from_dir(&dir).expect("应创建独立日志库");
        let secret = "sk-log-secret-must-not-persist";
        store
            .record_token_usage(&sample_usage(Some(serde_json::json!({
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 5},
                "api_key": secret,
                "provider_payload": {"authorization": secret}
            }))))
            .expect("应写入脱敏 usage");
        store
            .record_context_snapshot(&sample_snapshot(Some(serde_json::json!({
                "authorization": secret
            }))))
            .expect("应写入脱敏上下文快照");
        let rows = store.list_token_usage(None, None).expect("应读取 usage");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].raw_usage,
            Some(serde_json::json!({
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 5}
            }))
        );
        let snapshot = store
            .latest_context_snapshot("conversation-1")
            .expect("应读取上下文快照")
            .expect("上下文快照应存在");
        assert_eq!(snapshot.segments[0].metadata, None);

        for entry in std::fs::read_dir(dir.join("logs")).expect("应遍历日志目录") {
            let path = entry.expect("日志目录项应可读").path();
            if path.is_file() {
                let bytes = std::fs::read(&path).expect("日志文件应可读");
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(secret),
                    "日志数据库及 sidecar 不得包含秘密"
                );
            }
        }

        let core =
            rusqlite::Connection::open(dir.join("runtime/muse.sqlite")).expect("应打开核心库");
        for table in ["runtime_token_usage", "runtime_context_snapshots"] {
            let exists: bool = core
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .expect("应检查核心表");
            assert!(!exists, "核心库不得包含日志表 `{table}`");
        }
        drop(core);

        let backup_path = dir.join("backups/core.sqlite");
        crate::storage::backup_runtime_database(&dir, &backup_path).expect("应备份核心库");
        let backup = rusqlite::Connection::open(&backup_path).expect("应打开核心备份");
        let value: String = backup
            .query_row(
                "SELECT summary FROM session_index WHERE conversation_id = 'core-probe'",
                [],
                |row| row.get(0),
            )
            .expect("核心备份应包含核心状态");
        assert_eq!(value, "核心状态");
        let log_tables: i64 = backup
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('runtime_token_usage', 'runtime_context_snapshots')",
                [],
                |row| row.get(0),
            )
            .expect("应检查备份边界");
        assert_eq!(log_tables, 0, "核心备份不得夹带高频日志");
        drop(backup);

        let isolated_restore_dir = unique_temp_dir();
        std::fs::create_dir_all(isolated_restore_dir.join("runtime")).expect("应创建恢复目录");
        std::fs::copy(
            &backup_path,
            isolated_restore_dir.join("runtime/muse.sqlite"),
        )
        .expect("应复制仅含 runtime 的备份");
        let error = crate::storage::open_runtime_database(&isolated_restore_dir)
            .expect_err("缺失当前删除权威的独立目录不得开放带 anchor 的 runtime 备份");
        assert!(
            matches!(error, crate::storage::RuntimeStorageError::Integrity(_)),
            "缺失删除权威必须为 Integrity，实际为：{error}"
        );
        std::fs::remove_dir_all(isolated_restore_dir).expect("应清理独立恢复测试目录");

        for sidecar in [
            dir.join("runtime/muse.sqlite-wal"),
            dir.join("runtime/muse.sqlite-shm"),
        ] {
            if let Err(error) = std::fs::remove_file(&sidecar)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                panic!("应清理同安装恢复前的 SQLite sidecar：{error}");
            }
        }
        std::fs::copy(&backup_path, dir.join("runtime/muse.sqlite"))
            .expect("应在保留当前删除权威的同一安装恢复核心备份");
        let (_, restored) =
            crate::storage::open_runtime_database(&dir).expect("同安装恢复应通过统一校验");
        let restored_value: String = restored
            .query_row(
                "SELECT summary FROM session_index WHERE conversation_id = 'core-probe'",
                [],
                |row| row.get(0),
            )
            .expect("恢复库应包含核心状态");
        assert_eq!(restored_value, "核心状态");
        assert!(
            dir.join("logs/runtime-usage.sqlite").exists(),
            "同安装恢复核心备份不得回滚或删除独立日志库"
        );
        drop(restored);

        let before_clear =
            inspect_runtime_history_evidence_read_only(&dir).expect("应读取历史证据");
        assert!(before_clear.has_history);
        assert!(before_clear.unknown_tables.is_empty());
        store.clear().expect("应清理独立日志");
        assert!(
            store
                .list_token_usage(None, None)
                .expect("应读取空日志")
                .is_empty()
        );
        assert!(
            store
                .latest_context_snapshot("conversation-1")
                .expect("应读取空上下文日志")
                .is_none()
        );
        let after_clear = inspect_runtime_history_evidence_read_only(&dir).expect("应复查历史证据");
        assert!(!after_clear.has_history);

        for suffix in ["", "-wal", "-shm", "-journal"] {
            let path = dir.join(format!("logs/runtime-usage.sqlite{suffix}"));
            if path.exists() {
                std::fs::remove_file(path).expect("应删除可丢弃日志库");
            }
        }
        assert!(
            store
                .list_token_usage(None, None)
                .expect("删除日志库后应自动创建空库")
                .is_empty()
        );

        let core = rusqlite::Connection::open(dir.join("runtime/muse.sqlite"))
            .expect("清理日志后应继续打开核心库");
        let value: String = core
            .query_row(
                "SELECT summary FROM session_index WHERE conversation_id = 'core-probe'",
                [],
                |row| row.get(0),
            )
            .expect("日志清理不得影响核心状态");
        assert_eq!(value, "核心状态");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(dir.join("logs"))
                    .expect("应读取日志目录权限")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(store.database_path())
                    .expect("应读取日志文件权限")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(core);
        std::fs::remove_dir_all(dir).expect("应清理测试目录");
    }
}
