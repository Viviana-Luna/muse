//! 提供运行时统一存储路径、连接参数、显式迁移与耐久文件写入。

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, backup::Backup};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const RUNTIME_DATABASE_FILE: &str = "muse.sqlite";
const LEGACY_RUNTIME_DATABASE_FILE: &str = "agent-vp.sqlite";
const RUNTIME_DATABASE_DIR: &str = "runtime";
const RUNTIME_DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LATEST_RUNTIME_SCHEMA_MIGRATION: i64 = 9;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 统一运行时数据库初始化、连接或迁移错误。
#[derive(Debug)]
pub enum RuntimeStorageError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Migration(String),
    Integrity(String),
}

impl std::fmt::Display for RuntimeStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "运行时存储文件操作失败：{error}"),
            Self::Sqlite(error) => write!(f, "运行时 SQLite 操作失败：{error}"),
            Self::Migration(message) => write!(f, "运行时 SQLite 迁移失败：{message}"),
            Self::Integrity(message) => write!(f, "运行时 SQLite 完整性检查失败：{message}"),
        }
    }
}

impl std::error::Error for RuntimeStorageError {}

impl From<std::io::Error> for RuntimeStorageError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<rusqlite::Error> for RuntimeStorageError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

struct RuntimeMigration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const RUNTIME_MIGRATIONS: &[RuntimeMigration] = &[
    RuntimeMigration {
        version: 1,
        name: "storage_foundation",
        // 基础版本只发布统一 migration 与连接契约。业务表由各领域后续 migration
        // 明确创建，避免在共享层提前把声明式配置固化成 SQLite 事实源。
        sql: "",
    },
    RuntimeMigration {
        version: 2,
        name: "provider_profiles_to_config_toml",
        // 调用方必须先原子发布并回读 Provider Profile，再打开数据库触发本迁移。
        // Provider、模型列表、端点和凭据都不再允许在 SQLite 中保留平行副本。
        sql: r#"
            DROP TABLE IF EXISTS model_capabilities;
            DROP TABLE IF EXISTS catalog_meta;
            DROP TABLE IF EXISTS models;
            DROP TABLE IF EXISTS providers;
        "#,
    },
    RuntimeMigration {
        version: 3,
        name: "session_v3_rebuildable_index",
        // 会话正文与元数据事实只存在于 Session v3 JSONL。这里仅保存列表查询所需的
        // 有界投影和索引同步凭据，整张表可以随时从 JSONL 重建。
        sql: r#"
            CREATE TABLE session_index (
                conversation_id TEXT PRIMARY KEY,
                event_file TEXT NOT NULL,
                title TEXT,
                archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
                source_conversation_id TEXT,
                fallback_summary TEXT NOT NULL,
                summary TEXT NOT NULL,
                record_count INTEGER NOT NULL CHECK (record_count >= 0),
                created_time TEXT,
                last_time TEXT,
                metadata_updated_at TEXT,
                metadata_revision INTEGER NOT NULL DEFAULT 0 CHECK (metadata_revision >= 0),
                last_commit_seq INTEGER NOT NULL DEFAULT 0 CHECK (last_commit_seq >= 0)
            );
            CREATE INDEX session_index_last_time
                ON session_index(last_time DESC, conversation_id ASC);
            CREATE TABLE session_index_state (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                generation_id TEXT NOT NULL,
                manifest_hash TEXT NOT NULL,
                indexed_at TEXT NOT NULL
            );
        "#,
    },
    RuntimeMigration {
        version: 4,
        name: "runtime_logs_to_separate_database",
        // Token usage 与上下文快照属于可清理的高频运行日志，不能继续参与核心
        // muse.sqlite 的备份、恢复和生命周期。
        sql: r#"
            DROP TABLE IF EXISTS runtime_context_snapshots;
            DROP TABLE IF EXISTS runtime_token_usage;
        "#,
    },
    RuntimeMigration {
        version: 5,
        name: "session_index_recoverable_visibility",
        // 失败或尚未提交的回合仍保留在 JSONL 中用于审计，但不能出现在“可恢复会话”
        // 列表。旧索引没有该投影维度，迁移后清空可重建索引，强制从 JSONL 重算。
        sql: r#"
            ALTER TABLE session_index
                ADD COLUMN recoverable INTEGER NOT NULL DEFAULT 0
                CHECK (recoverable IN (0, 1));
            DELETE FROM session_index;
            DELETE FROM session_index_state;
        "#,
    },
    RuntimeMigration {
        version: 6,
        name: "persona_session_ownership_v2",
        // 当前没有真实用户，Session metadata 直接硬切 v2。旧 SQLite 投影全部
        // 清空，不保留 v1 双读或未绑定会话认领分支。
        sql: r#"
            ALTER TABLE session_index ADD COLUMN persona_id TEXT;
            ALTER TABLE session_index ADD COLUMN persona_name_snapshot TEXT;
            ALTER TABLE session_index ADD COLUMN persona_version_snapshot TEXT;
            CREATE INDEX session_index_persona_last_time
                ON session_index(persona_id, last_time DESC, conversation_id ASC);
            CREATE TABLE persona_workspace_state (
                persona_id TEXT PRIMARY KEY,
                active_conversation_id TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            DELETE FROM session_index;
            DELETE FROM session_index_state;
        "#,
    },
    RuntimeMigration {
        version: 7,
        name: "persona_emotion_state_mvp",
        // Session v3 的 turn_committed payload 仍是人格状态的 canonical 事实。
        // event 表提供按 turn_id 幂等的结构化审计，projection 只保存当前状态，
        // 两者均不承载记忆、关系、成长或任何权限事实。
        sql: r#"
            CREATE TABLE persona_state_event (
                turn_id TEXT PRIMARY KEY,
                persona_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                source_commit_seq INTEGER NOT NULL UNIQUE CHECK (source_commit_seq > 0),
                emotion TEXT NOT NULL CHECK (
                    emotion IN ('happy', 'sad', 'surprised', 'thinking', 'neutral', 'angry')
                ),
                intensity INTEGER NOT NULL CHECK (intensity BETWEEN 0 AND 100),
                reason_code TEXT NOT NULL CHECK (
                    reason_code IN (
                        'positive_interaction', 'negative_interaction', 'surprise',
                        'deliberation', 'conflict', 'neutral'
                    )
                ),
                committed_at TEXT NOT NULL
            );
            CREATE INDEX persona_state_event_persona_revision
                ON persona_state_event(persona_id, source_commit_seq DESC);
            CREATE TABLE persona_state_projection (
                persona_id TEXT PRIMARY KEY,
                emotion TEXT NOT NULL CHECK (
                    emotion IN ('happy', 'sad', 'surprised', 'thinking', 'neutral', 'angry')
                ),
                intensity INTEGER NOT NULL CHECK (intensity BETWEEN 0 AND 100),
                reason_code TEXT NOT NULL CHECK (
                    reason_code IN (
                        'positive_interaction', 'negative_interaction', 'surprise',
                        'deliberation', 'conflict', 'neutral'
                    )
                ),
                source_conversation_id TEXT NOT NULL,
                source_turn_id TEXT NOT NULL,
                last_interaction_at TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK (revision > 0)
            );
        "#,
    },
    RuntimeMigration {
        version: 8,
        name: "persona_long_term_memory_storage",
        // 记忆正文与全部可重建投影归入统一 runtime SQLite；删除权威位于独立
        // 恢复域，不在本 migration 中建表。所有业务键都包含 persona_id，
        // current 指针与 revision 使用延迟外键保证一次事务内原子切换。
        sql: r#"
            CREATE TABLE memory_authority_anchor (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                authority_id TEXT NOT NULL,
                initialized_at TEXT NOT NULL,
                last_applied_revision INTEGER NOT NULL DEFAULT 0
                    CHECK (last_applied_revision >= 0)
            );

            CREATE TABLE memory_entry (
                persona_id TEXT NOT NULL,
                memory_id TEXT NOT NULL,
                category TEXT NOT NULL CHECK (
                    category IN (
                        'user_fact', 'user_preference', 'shared_experience',
                        'commitment', 'story_state'
                    )
                ),
                current_revision_id TEXT NOT NULL,
                importance TEXT NOT NULL CHECK (importance IN ('low', 'normal', 'high')),
                freshness_at TEXT NOT NULL,
                created_at TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('active', 'deleted')),
                PRIMARY KEY(persona_id, memory_id),
                UNIQUE(persona_id, memory_id, current_revision_id),
                FOREIGN KEY(persona_id, memory_id, current_revision_id)
                    REFERENCES memory_revision(persona_id, memory_id, revision_id)
                    DEFERRABLE INITIALLY DEFERRED
            );
            CREATE INDEX memory_entry_persona_state_freshness
                ON memory_entry(persona_id, state, freshness_at DESC, memory_id ASC);

            CREATE TABLE memory_revision (
                row_id INTEGER PRIMARY KEY,
                persona_id TEXT NOT NULL,
                memory_id TEXT NOT NULL,
                revision_id TEXT NOT NULL,
                content TEXT NOT NULL,
                derivation_key BLOB NOT NULL CHECK (length(derivation_key) = 32),
                event_time TEXT,
                recorded_at TEXT NOT NULL,
                valid_from TEXT NOT NULL,
                valid_to TEXT,
                change_type TEXT NOT NULL CHECK (
                    change_type IN ('create', 'update', 'correct')
                ),
                change_reason TEXT NOT NULL,
                safety_policy_version TEXT NOT NULL,
                state TEXT NOT NULL CHECK (
                    state IN ('current', 'superseded', 'corrected')
                ),
                UNIQUE(persona_id, memory_id, revision_id),
                FOREIGN KEY(persona_id, memory_id)
                    REFERENCES memory_entry(persona_id, memory_id)
                    ON DELETE CASCADE
                    DEFERRABLE INITIALLY DEFERRED
            );
            CREATE INDEX memory_revision_persona_memory_state
                ON memory_revision(persona_id, memory_id, state, valid_from DESC);
            CREATE INDEX memory_revision_persona_derivation
                ON memory_revision(persona_id, derivation_key);

            CREATE TABLE memory_revision_source (
                persona_id TEXT NOT NULL,
                memory_id TEXT NOT NULL,
                revision_id TEXT NOT NULL,
                source_ordinal INTEGER NOT NULL CHECK (source_ordinal >= 0),
                source_kind TEXT NOT NULL CHECK (
                    source_kind IN (
                        'direct_user_message', 'user_confirmation',
                        'deterministic_local_event', 'persona_management'
                    )
                ),
                conversation_id TEXT,
                turn_id TEXT,
                action_id TEXT,
                authorized_at TEXT,
                PRIMARY KEY(persona_id, memory_id, revision_id, source_ordinal),
                FOREIGN KEY(persona_id, memory_id, revision_id)
                    REFERENCES memory_revision(persona_id, memory_id, revision_id)
                    ON DELETE CASCADE,
                CHECK (
                    (
                        source_kind IN (
                            'direct_user_message', 'user_confirmation',
                            'deterministic_local_event'
                        )
                        AND conversation_id IS NOT NULL
                        AND turn_id IS NOT NULL
                        AND action_id IS NULL
                        AND authorized_at IS NULL
                    )
                    OR (
                        source_kind = 'persona_management'
                        AND conversation_id IS NULL
                        AND turn_id IS NULL
                        AND action_id IS NOT NULL
                        AND authorized_at IS NOT NULL
                    )
                )
            );
            CREATE INDEX memory_revision_source_persona_turn
                ON memory_revision_source(persona_id, conversation_id, turn_id);

            CREATE TABLE memory_committed_batch (
                persona_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                envelope_digest BLOB NOT NULL CHECK (length(envelope_digest) = 32),
                durable_at TEXT NOT NULL,
                PRIMARY KEY(persona_id, idempotency_key),
                UNIQUE(persona_id, conversation_id, turn_id)
            );
            CREATE TABLE memory_committed_operation (
                persona_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                operation_ordinal INTEGER NOT NULL CHECK (operation_ordinal >= 0),
                operation_id TEXT NOT NULL,
                change_type TEXT NOT NULL CHECK (
                    change_type IN ('create', 'update', 'correct')
                ),
                memory_id TEXT NOT NULL,
                revision_id TEXT NOT NULL,
                PRIMARY KEY(persona_id, idempotency_key, operation_ordinal),
                UNIQUE(persona_id, operation_id),
                FOREIGN KEY(persona_id, idempotency_key)
                    REFERENCES memory_committed_batch(persona_id, idempotency_key)
                    ON DELETE CASCADE
            );

            CREATE TABLE memory_management_operation (
                persona_id TEXT NOT NULL,
                operation_id TEXT NOT NULL,
                operation_kind TEXT NOT NULL CHECK (
                    operation_kind IN ('content', 'importance')
                ),
                mutation_digest BLOB NOT NULL CHECK (length(mutation_digest) = 32),
                change_type TEXT CHECK (
                    change_type IS NULL OR change_type IN ('create', 'correct')
                ),
                memory_id TEXT NOT NULL,
                revision_id TEXT,
                previous_importance TEXT CHECK (
                    previous_importance IS NULL
                    OR previous_importance IN ('low', 'normal', 'high')
                ),
                importance TEXT CHECK (
                    importance IS NULL OR importance IN ('low', 'normal', 'high')
                ),
                durable_at TEXT NOT NULL,
                PRIMARY KEY(persona_id, operation_id),
                CHECK (
                    (
                        operation_kind = 'content'
                        AND change_type IS NOT NULL
                        AND revision_id IS NOT NULL
                        AND previous_importance IS NULL
                        AND importance IS NULL
                    )
                    OR (
                        operation_kind = 'importance'
                        AND change_type IS NULL
                        AND revision_id IS NULL
                        AND previous_importance IS NOT NULL
                        AND importance IS NOT NULL
                    )
                )
            );

            CREATE TABLE memory_search_projection (
                row_id INTEGER PRIMARY KEY,
                persona_id TEXT NOT NULL,
                memory_id TEXT NOT NULL,
                revision_id TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT NOT NULL CHECK (
                    category IN (
                        'user_fact', 'user_preference', 'shared_experience',
                        'commitment', 'story_state'
                    )
                ),
                UNIQUE(persona_id, memory_id),
                FOREIGN KEY(persona_id, memory_id, revision_id)
                    REFERENCES memory_revision(persona_id, memory_id, revision_id)
                    ON DELETE CASCADE
            );
            CREATE VIRTUAL TABLE memory_fts USING fts5(
                persona_id UNINDEXED,
                memory_id UNINDEXED,
                revision_id UNINDEXED,
                content,
                category,
                content = 'memory_search_projection',
                content_rowid = 'row_id',
                tokenize = 'trigram'
            );
            INSERT INTO memory_fts(memory_fts, rank) VALUES('secure-delete', 1);
            CREATE TRIGGER memory_search_projection_insert
            AFTER INSERT ON memory_search_projection BEGIN
                INSERT INTO memory_fts(
                    rowid, persona_id, memory_id, revision_id, content, category
                ) VALUES(
                    new.row_id, new.persona_id, new.memory_id, new.revision_id,
                    new.content, new.category
                );
            END;
            CREATE TRIGGER memory_search_projection_delete
            AFTER DELETE ON memory_search_projection BEGIN
                INSERT INTO memory_fts(
                    memory_fts, rowid, persona_id, memory_id, revision_id, content, category
                ) VALUES(
                    'delete', old.row_id, old.persona_id, old.memory_id, old.revision_id,
                    old.content, old.category
                );
            END;
            CREATE TRIGGER memory_search_projection_update
            AFTER UPDATE ON memory_search_projection BEGIN
                INSERT INTO memory_fts(
                    memory_fts, rowid, persona_id, memory_id, revision_id, content, category
                ) VALUES(
                    'delete', old.row_id, old.persona_id, old.memory_id, old.revision_id,
                    old.content, old.category
                );
                INSERT INTO memory_fts(
                    rowid, persona_id, memory_id, revision_id, content, category
                ) VALUES(
                    new.row_id, new.persona_id, new.memory_id, new.revision_id,
                    new.content, new.category
                );
            END;
        "#,
    },
    RuntimeMigration {
        version: 9,
        name: "memory_revision_attribute_snapshots",
        // category 与 importance 属于 revision 当时的检索语义。旧 v8 库无法从
        // current entry 反推出历史值，因此升级前只接受空 revision 表；有数据时
        // 由迁移前置检查 fail closed，禁止伪造历史快照。
        sql: r#"
            ALTER TABLE memory_revision
                ADD COLUMN category TEXT NOT NULL CHECK (
                    category IN (
                        'user_fact', 'user_preference', 'shared_experience',
                        'commitment', 'story_state'
                    )
                );
            ALTER TABLE memory_revision
                ADD COLUMN importance TEXT NOT NULL CHECK (
                    importance IN ('low', 'normal', 'high')
                );
        "#,
    },
];

/// 打开指定数据目录的统一运行时库，并完成连接配置与显式 migration。
pub fn open_runtime_database(
    base_dir: impl AsRef<Path>,
) -> Result<(PathBuf, Connection), RuntimeStorageError> {
    let path = runtime_database_path(base_dir);
    let connection = open_runtime_database_at_path(&path)?;
    Ok((path, connection))
}

/// 打开已知运行时数据库路径；所有读写连接必须经过同一参数入口。
pub fn open_runtime_database_at_path(path: &Path) -> Result<Connection, RuntimeStorageError> {
    prepare_runtime_database_path(path)?;
    let mut connection = Connection::open(path)?;
    // 数据库可能由旧版本以默认 umask 创建；必须在启用 WAL 前修复主文件权限，
    // 让随后生成的辅助文件继承同等级的私有访问边界。
    restrict_sensitive_file_permissions(path)?;
    configure_runtime_connection(&connection)?;
    run_runtime_migrations(&mut connection, path)?;
    crate::app::memory_storage::reconcile_runtime_memory(&mut connection, path)?;
    Ok(connection)
}

/// 已完成 migration 与恢复协调的记忆 Repository 使用的轻量连接入口。
///
/// Repository 在持有删除权威事务时不能递归触发恢复；构造 Repository 时已经通过
/// 正式入口完成迁移、anchor 校验和权威重放，后续同一实例只复用统一连接参数。
pub(crate) fn open_initialized_runtime_database(
    base_dir: impl AsRef<Path>,
) -> Result<Connection, RuntimeStorageError> {
    let path = runtime_database_path(base_dir);
    prepare_runtime_database_path(&path)?;
    let connection = Connection::open(&path)?;
    restrict_sensitive_file_permissions(&path)?;
    configure_runtime_connection(&connection)?;
    Ok(connection)
}

/// 在 SQLite 打开数据库前建立统一的私有目录与文件边界。
///
/// 旧模型目录迁移需要先读取已退场的业务表，不能提前运行统一 migration；该入口让它
/// 仍与正式运行时连接共享相同的路径和权限检查。
pub fn prepare_runtime_database_path(path: &Path) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("运行时数据库路径缺少父目录"))?;
    ensure_private_runtime_directory(parent)?;
    reject_non_regular_file_path(path)?;
    restrict_sensitive_file_permissions(path)
}

fn configure_runtime_connection(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.busy_timeout(RUNTIME_DATABASE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "secure_delete", "ON")?;
    Ok(())
}

fn run_runtime_migrations(
    connection: &mut Connection,
    database_path: &Path,
) -> Result<(), RuntimeStorageError> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY CHECK (version > 0),
            name TEXT NOT NULL UNIQUE,
            applied_at TEXT NOT NULL
        );
        "#,
    )?;
    validate_runtime_schema_shape(connection)?;
    let newest: Option<i64> =
        connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?;
    if let Some(version) = newest
        && version > LATEST_RUNTIME_SCHEMA_MIGRATION
    {
        return Err(RuntimeStorageError::Migration(format!(
            "数据库 migration 版本 {version} 高于当前程序支持的 {LATEST_RUNTIME_SCHEMA_MIGRATION}"
        )));
    }
    if newest.is_some_and(|version| version >= 8) {
        validate_memory_authority_anchor(connection, database_path)?;
    }

    // migration 8 的删除权威准备必须先于任何主库 IMMEDIATE 事务完成：权威初始化
    // 会对权威库 BEGIN IMMEDIATE，全局锁序固定为 authority -> main，主库事务内
    // 再触权威库会形成反向 main -> authority 边。anchor 播种仍留在 migration 8
    // 的主库事务内，与 schema 变更保持原子。
    let pending_authority_anchor = if newest.is_none_or(|version| version < 8) {
        let base_dir = runtime_database_base_dir(database_path)?;
        Some(
            crate::app::memory_storage::prepare_authority_for_migration(base_dir).map_err(
                |_| {
                    RuntimeStorageError::Integrity(
                        "记忆删除权威不可用，已拒绝执行 migration 8".to_string(),
                    )
                },
            )?,
        )
    } else {
        None
    };

    for migration in RUNTIME_MIGRATIONS {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let applied_name: Option<String> = transaction
            .query_row(
                "SELECT name FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(applied_name) = applied_name {
            if applied_name != migration.name {
                return Err(RuntimeStorageError::Migration(format!(
                    "migration {} 名称不一致：数据库为 `{applied_name}`，程序为 `{}`",
                    migration.version, migration.name
                )));
            }
            transaction.commit()?;
            continue;
        }
        if migration.version == 9 {
            let revision_count: i64 =
                transaction
                    .query_row("SELECT COUNT(*) FROM memory_revision", [], |row| row.get(0))?;
            if revision_count != 0 {
                return Err(RuntimeStorageError::Integrity(
                    "旧版记忆 revision 缺少 category/importance 历史快照，已拒绝自动升级"
                        .to_string(),
                ));
            }
        }
        transaction.execute_batch(migration.sql)?;
        if migration.version == 8 {
            let anchor = pending_authority_anchor.as_ref().ok_or_else(|| {
                RuntimeStorageError::Integrity(
                    "记忆删除权威不可用，已拒绝执行 migration 8".to_string(),
                )
            })?;
            transaction.execute(
                "INSERT INTO memory_authority_anchor(
                    singleton, authority_id, initialized_at, last_applied_revision
                 ) VALUES(1, ?1, ?2, ?3)",
                rusqlite::params![
                    anchor.authority_id,
                    anchor.initialized_at,
                    anchor.last_applied_revision
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO schema_migrations(version, name, applied_at)
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            rusqlite::params![migration.version, migration.name],
        )?;
        validate_runtime_database(&transaction)?;
        transaction.commit()?;
    }
    validate_runtime_schema_shape(connection)?;
    validate_memory_authority_anchor(connection, database_path)?;
    Ok(())
}

fn runtime_database_base_dir(database_path: &Path) -> Result<&Path, RuntimeStorageError> {
    database_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            RuntimeStorageError::Integrity("运行时数据库路径无法定位记忆删除权威恢复域".to_string())
        })
}

fn validate_memory_authority_anchor(
    connection: &Connection,
    database_path: &Path,
) -> Result<(), RuntimeStorageError> {
    let anchor = connection
        .query_row(
            "SELECT authority_id, initialized_at, last_applied_revision
             FROM memory_authority_anchor
             WHERE singleton = 1",
            [],
            |row| {
                Ok(crate::app::memory_storage::AuthorityAnchor {
                    authority_id: row.get(0)?,
                    initialized_at: row.get(1)?,
                    last_applied_revision: row.get(2)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            RuntimeStorageError::Integrity(
                "migration 8 已存在，但记忆删除权威 anchor 缺失".to_string(),
            )
        })?;
    let base_dir = runtime_database_base_dir(database_path)?;
    crate::app::memory_storage::open_authority_for_anchor(base_dir, &anchor).map_err(|_| {
        RuntimeStorageError::Integrity(
            "记忆删除权威与 runtime anchor 不匹配，已拒绝开放数据库".to_string(),
        )
    })?;
    Ok(())
}

fn validate_runtime_schema_shape(connection: &Connection) -> Result<(), RuntimeStorageError> {
    let mut statement = connection.prepare("PRAGMA table_info(schema_migrations)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns != ["version", "name", "applied_at"] {
        return Err(RuntimeStorageError::Integrity(
            "schema_migrations 表结构不符合统一 migration 契约".to_string(),
        ));
    }
    Ok(())
}

fn validate_runtime_database(connection: &Connection) -> Result<(), RuntimeStorageError> {
    validate_runtime_schema_shape(connection)?;
    let quick_check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(RuntimeStorageError::Integrity(format!(
            "SQLite quick_check 返回：{quick_check}"
        )));
    }
    let mut foreign_key_check = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign_key_check.query([])?.next()?.is_some() {
        return Err(RuntimeStorageError::Integrity(
            "SQLite foreign_key_check 发现无效引用".to_string(),
        ));
    }
    Ok(())
}

fn ensure_regular_directory(path: &Path) -> Result<(), std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            std::io::Error::other(format!("路径 `{}` 不是普通目录", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(std::io::Error::other(format!(
                    "创建后的路径 `{}` 不是普通目录",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn ensure_private_runtime_directory(path: &Path) -> Result<(), std::io::Error> {
    ensure_regular_directory(path)?;
    set_sensitive_directory_permissions(path)
}

#[cfg(unix)]
fn set_sensitive_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_sensitive_directory_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn reject_non_regular_file_path(path: &Path) -> Result<(), std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            std::io::Error::other(format!("文件路径 `{}` 不是普通文件", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// 将小型事实文件耐久地原子发布到目标路径。
///
/// 写入临时文件并完成文件同步后才替换目标；任何失败都会清理临时文件并保留旧事实。
pub fn atomic_write_synced(path: &Path, content: &[u8]) -> Result<(), std::io::Error> {
    atomic_write_synced_with_permissions(path, content, false)
}

/// 将包含 API Key 等秘密的小型配置耐久发布，并在写入秘密前收紧文件权限。
pub fn atomic_write_sensitive_synced(path: &Path, content: &[u8]) -> Result<(), std::io::Error> {
    atomic_write_synced_with_permissions(path, content, true)
}

fn atomic_write_synced_with_permissions(
    path: &Path,
    content: &[u8],
    sensitive: bool,
) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("耐久文件路径缺少父目录"))?;
    ensure_regular_directory(parent)?;
    reject_non_regular_file_path(path)?;
    let temporary = create_unique_temporary_path(path)?;
    let result = (|| {
        if sensitive {
            restrict_sensitive_file_permissions(&temporary)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent_directory(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 确保已存在的敏感配置或数据库不会继续沿用过宽的 Unix 文件权限。
///
/// Windows 侧依赖数据目录在创建时显式收紧的受保护 DACL：目录内文件自动继承
/// 仅含当前用户、SYSTEM 与 Administrators 的 ACE，无需逐文件处理。
pub fn restrict_sensitive_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    reject_non_regular_file_path(path)?;
    if !path.exists() {
        return Ok(());
    }
    set_sensitive_file_permissions(path)
}

#[cfg(unix)]
fn set_sensitive_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_sensitive_file_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

/// 使用 SQLite Backup API 为统一运行时库创建可恢复快照。
///
/// 源库保持 WAL 模式时也不得直接复制主文件，否则最近已提交但尚未 checkpoint 的数据
/// 会从备份中消失。该入口先写入同目录临时库、执行 quick_check，再原子发布目标文件。
pub fn backup_runtime_database(
    base_dir: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<PathBuf, RuntimeStorageError> {
    let source_path = runtime_database_path(base_dir);
    reject_non_regular_file_path(&source_path)?;
    if !source_path.is_file() {
        return Err(RuntimeStorageError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("运行时数据库 `{}` 不存在", source_path.display()),
        )));
    }
    let destination = destination.as_ref();
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("数据库备份路径缺少父目录"))?;
    ensure_regular_directory(parent)?;
    reject_non_regular_file_path(destination)?;
    let temporary = create_unique_temporary_path(destination)?;

    let result = (|| {
        restrict_sensitive_file_permissions(&temporary)?;
        let source = open_runtime_database_at_path(&source_path)?;
        let mut target = Connection::open(&temporary)?;
        Backup::new(&source, &mut target)?.run_to_completion(
            64,
            Duration::from_millis(10),
            None,
        )?;
        drop(target);
        drop(source);
        validate_database_for_storage(&temporary)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        replace_file(&temporary, destination)?;
        sync_parent_directory(parent);
        Ok(destination.to_path_buf())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn create_unique_temporary_path(destination: &Path) -> Result<PathBuf, std::io::Error> {
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("临时数据库路径缺少父目录"))?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("muse-data");
    for _ in 0..32 {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{file_name}.{}.{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => {
                drop(file);
                return Ok(temporary);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "无法创建唯一的耐久写入临时文件",
    ))
}

fn validate_database_for_storage(path: &Path) -> Result<(), RuntimeStorageError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    validate_runtime_database(&connection)
}

#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    fs::rename(source, destination)
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    for attempt in 0..20 {
        // SAFETY: 两个 UTF-16 缓冲区均以 NUL 结尾并在调用期间有效。
        let result = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        let transient_sharing_conflict = matches!(error.raw_os_error(), Some(5 | 32 | 33));
        if !transient_sharing_conflict || attempt == 19 {
            return Err(error);
        }
        // Windows 的文件替换会短暂撞上 SQLite、杀毒软件或并发发布持有的句柄。
        // 有界重试只吸收共享冲突，不掩盖路径、ACL 等其他真实错误。
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) {
    if let Err(error) = sync_parent_directory_required(parent) {
        tracing::warn!(
            path = %parent.display(),
            %error,
            "耐久文件已原子发布，但父目录同步失败"
        );
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) {}

/// 隐私权威等前向单调事实必须把父目录同步失败视为提交失败。
#[cfg(unix)]
pub(crate) fn sync_parent_directory_required(parent: &Path) -> Result<(), std::io::Error> {
    File::open(parent).and_then(|directory| directory.sync_all())
}

#[cfg(not(unix))]
pub(crate) fn sync_parent_directory_required(_parent: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

/// 运行时统一 SQLite 数据库路径。
///
/// 文本 JSON 配置和大体积模型文件仍按各自目录保存；可结构化查询的目录、音色等
/// 运行时索引统一收口到这个库，避免后续到处散落多个 SQLite 文件。
pub fn runtime_database_path(base_dir: impl AsRef<Path>) -> PathBuf {
    let base_dir = base_dir.as_ref();
    let runtime_dir = base_dir.join(RUNTIME_DATABASE_DIR);
    let current = runtime_dir.join(RUNTIME_DATABASE_FILE);
    let legacy = runtime_dir.join(LEGACY_RUNTIME_DATABASE_FILE);
    if current.exists() {
        let migration_completed = base_dir
            .join(".muse-migrations/legacy-workspace-v1.json")
            .is_file();
        if migration_completed && legacy.is_file() {
            match validate_database(&current) {
                Ok(()) => {
                    if let Err(error) = fs::remove_file(&legacy) {
                        tracing::warn!(%error, legacy = %legacy.display(), "无法清理已迁移的旧数据库文件");
                    }
                }
                Err(error) => {
                    tracing::error!(
                        %error,
                        current = %current.display(),
                        legacy = %legacy.display(),
                        "Muse 数据库完整性校验失败，保留并继续使用旧数据库"
                    );
                    return legacy;
                }
            }
        }
        return current;
    }

    if legacy.is_file()
        && let Err(error) = copy_legacy_database(&legacy, &current)
    {
        tracing::warn!(
            %error,
            legacy = %legacy.display(),
            "无法复制 legacy 运行时数据库，当前进程继续使用旧文件"
        );
        return legacy;
    }
    current
}

/// 旧副本只在新数据库可读且通过 SQLite 快速完整性检查后删除。
fn validate_database(path: &Path) -> Result<(), String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("只读打开数据库失败：{error}"))?;
    let result: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| format!("执行数据库完整性校验失败：{error}"))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(format!("SQLite quick_check 返回：{result}"))
    }
}

/// 通过 SQLite backup API 复制旧数据库并原子发布新名称，成功后清除当前数据目录内的旧副本。
fn copy_legacy_database(legacy: &Path, current: &Path) -> Result<(), String> {
    let parent = current
        .parent()
        .ok_or_else(|| "运行时数据库缺少父目录".to_string())?;
    ensure_private_runtime_directory(parent)
        .map_err(|error| format!("创建或修复运行时目录失败：{error}"))?;
    restrict_sensitive_file_permissions(legacy)
        .map_err(|error| format!("修复 legacy 数据库权限失败：{error}"))?;
    let temporary = create_unique_temporary_path(current)
        .map_err(|error| format!("创建临时 Muse 数据库失败：{error}"))?;
    let result = (|| {
        restrict_sensitive_file_permissions(&temporary)
            .map_err(|error| format!("修复临时 Muse 数据库权限失败：{error}"))?;
        let source = Connection::open_with_flags(legacy, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("打开 legacy 数据库失败：{error}"))?;
        let mut target = Connection::open(&temporary)
            .map_err(|error| format!("创建临时 Muse 数据库失败：{error}"))?;
        Backup::new(&source, &mut target)
            .and_then(|backup| backup.run_to_completion(64, Duration::from_millis(10), None))
            .map_err(|error| format!("备份 legacy 数据库失败：{error}"))?;
        drop(target);
        drop(source);
        validate_database(&temporary)
            .map_err(|error| format!("临时 Muse 数据库校验失败：{error}"))?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("同步临时 Muse 数据库失败：{error}"))?;
        replace_file(&temporary, current)
            .map_err(|error| format!("原子发布 Muse 数据库失败：{error}"))?;
        sync_parent_directory(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return result;
    }
    if let Err(error) = fs::remove_file(legacy) {
        tracing::warn!(%error, legacy = %legacy.display(), "新数据库已发布，但清理旧数据库失败");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path)
            .expect("应读取文件权限")
            .permissions()
            .mode()
            & 0o777
    }

    fn unique_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "muse-storage-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos()
        ))
    }

    #[test]
    fn opens_runtime_database_with_versioned_schema_and_required_pragmas() {
        let root = unique_root("foundation");
        let (_, connection) = super::open_runtime_database(&root).expect("应初始化统一运行时库");

        let migration: (i64, String) = connection
            .query_row(
                "SELECT version, name FROM schema_migrations ORDER BY version DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("应记录最新 migration");
        assert_eq!(
            migration,
            (9, "memory_revision_attribute_snapshots".to_string())
        );
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'schema_migrations'",
                [],
                |row| row.get(0),
            )
            .expect("应查询 migration 表");
        assert_eq!(tables, 1);
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("应读取外键配置");
        let busy_timeout: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("应读取锁等待配置");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("应读取日志模式");
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("应读取同步级别");
        let secure_delete: i64 = connection
            .query_row("PRAGMA secure_delete", [], |row| row.get(0))
            .expect("应读取安全删除配置");
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5_000);
        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 1);
        assert_eq!(secure_delete, 1);
        drop(connection);

        let (_, reopened) = super::open_runtime_database(&root).expect("重复启动应保持幂等");
        let migration_count: i64 = reopened
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("应读取 migration 数量");
        assert_eq!(migration_count, 9);
        let recoverable_column: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_index') WHERE name = 'recoverable'",
                [],
                |row| row.get(0),
            )
            .expect("应检查会话可恢复投影列");
        assert_eq!(recoverable_column, 1);
        drop(reopened);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[cfg(unix)]
    #[test]
    fn repairs_existing_runtime_directory_and_database_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_root("private-permissions");
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).expect("应创建运行时目录");
        let database = runtime.join("muse.sqlite");
        drop(Connection::open(&database).expect("应创建旧版运行时数据库"));
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755))
            .expect("应放宽测试目录权限");
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o644))
            .expect("应放宽测试数据库权限");

        let (opened_path, connection) =
            super::open_runtime_database(&root).expect("启动应修复旧版存储权限");

        assert_eq!(opened_path, database);
        assert_eq!(unix_mode(&runtime), 0o700);
        assert_eq!(unix_mode(&database), 0o600);
        connection
            .execute_batch(
                "CREATE TABLE permission_probe(value TEXT NOT NULL);
                 INSERT INTO permission_probe VALUES('private');",
            )
            .expect("应写入 WAL 权限探针");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("应读取日志模式");
        assert_eq!(journal_mode, "wal");
        let wal = runtime.join("muse.sqlite-wal");
        assert!(wal.is_file(), "测试期间应保留 WAL 文件");
        assert_eq!(unix_mode(&wal), 0o600);
        let shared_memory = runtime.join("muse.sqlite-shm");
        if shared_memory.is_file() {
            assert_eq!(unix_mode(&shared_memory), 0o600);
        }
        drop(connection);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn migration_four_removes_high_frequency_log_tables_from_core_database() {
        let root = unique_root("remove-runtime-logs");
        let (_, connection) = super::open_runtime_database(&root).expect("应初始化核心库");
        connection
            .execute_batch(
                "DELETE FROM schema_migrations WHERE version = 4;
                 CREATE TABLE runtime_token_usage(id TEXT PRIMARY KEY);
                 CREATE TABLE runtime_context_snapshots(turn_id TEXT PRIMARY KEY);
                 INSERT INTO runtime_token_usage VALUES('legacy-usage');
                 INSERT INTO runtime_context_snapshots VALUES('legacy-turn');",
            )
            .expect("应模拟旧核心日志表");
        drop(connection);

        let (_, migrated) = super::open_runtime_database(&root).expect("应执行日志隔离 migration");
        for table in ["runtime_token_usage", "runtime_context_snapshots"] {
            let exists: bool = migrated
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .expect("应检查日志表退场");
            assert!(!exists, "核心库不应继续保留日志表 `{table}`");
        }
        let latest: i64 = migrated
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("应读取最新 migration");
        assert_eq!(latest, 9);
        drop(migrated);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn migration_five_invalidates_old_session_index_for_rebuild() {
        let root = unique_root("session-recoverable-index");
        let (_, connection) = super::open_runtime_database(&root).expect("应初始化核心库");
        connection
            .execute_batch(
                "INSERT INTO session_index(
                     conversation_id, event_file, title, archived, source_conversation_id,
                     fallback_summary, summary, record_count, created_time, last_time,
                     metadata_updated_at, metadata_revision, last_commit_seq, recoverable
                 ) VALUES(
                     'stale-chat', 'sessions/stale.jsonl', NULL, 0, NULL,
                     '未命名会话', '未命名会话', 1, NULL, NULL, NULL, 0, 1, 0
                 );
                 INSERT INTO session_index_state(singleton, generation_id, manifest_hash, indexed_at)
                 VALUES(1, 'stale-generation', 'stale-hash', 'now');
                 DELETE FROM schema_migrations WHERE version = 5;
                 ALTER TABLE session_index DROP COLUMN recoverable;",
            )
            .expect("应模拟 migration v4 的旧会话索引");
        drop(connection);

        let (_, migrated) = super::open_runtime_database(&root).expect("应执行会话可恢复投影迁移");
        let indexed_rows: i64 = migrated
            .query_row("SELECT COUNT(*) FROM session_index", [], |row| row.get(0))
            .expect("应读取会话索引行数");
        let index_state_rows: i64 = migrated
            .query_row("SELECT COUNT(*) FROM session_index_state", [], |row| {
                row.get(0)
            })
            .expect("应读取会话索引状态行数");
        assert_eq!(indexed_rows, 0);
        assert_eq!(index_state_rows, 0);
        drop(migrated);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn migration_six_rebuilds_persona_session_projection_and_workspace_state() {
        let root = unique_root("persona-session-ownership");
        let (_, connection) = super::open_runtime_database(&root).expect("应初始化核心库");
        connection
            .execute_batch(
                "INSERT INTO session_index(
                     conversation_id, event_file, title, archived, source_conversation_id,
                     fallback_summary, summary, record_count, created_time, last_time,
                     metadata_updated_at, metadata_revision, last_commit_seq, recoverable,
                     persona_id, persona_name_snapshot, persona_version_snapshot
                 ) VALUES(
                     'stale-chat', 'sessions/stale.jsonl', NULL, 0, NULL,
                     '未命名会话', '未命名会话', 1, NULL, NULL, NULL, 0, 1, 1,
                     'persona-a', '角色 A', '1.0.0'
                 );
                 INSERT INTO persona_workspace_state(persona_id, active_conversation_id, updated_at)
                 VALUES('persona-a', 'stale-chat', 'now');
                 DELETE FROM schema_migrations WHERE version = 6;
                 DROP TABLE persona_workspace_state;
                 DROP INDEX session_index_persona_last_time;
                 ALTER TABLE session_index DROP COLUMN persona_version_snapshot;
                 ALTER TABLE session_index DROP COLUMN persona_name_snapshot;
                 ALTER TABLE session_index DROP COLUMN persona_id;",
            )
            .expect("应模拟 migration v5 的旧会话索引");
        drop(connection);

        let (_, migrated) = super::open_runtime_database(&root).expect("应执行角色会话归属迁移");
        let indexed_rows: i64 = migrated
            .query_row("SELECT COUNT(*) FROM session_index", [], |row| row.get(0))
            .expect("应读取重建后的会话索引");
        let persona_columns: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_index')
                 WHERE name IN ('persona_id', 'persona_name_snapshot', 'persona_version_snapshot')",
                [],
                |row| row.get(0),
            )
            .expect("应检查 Persona 投影列");
        let workspace_table: bool = migrated
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'persona_workspace_state')",
                [],
                |row| row.get(0),
            )
            .expect("应检查角色工作区状态表");
        assert_eq!(indexed_rows, 0);
        assert_eq!(persona_columns, 3);
        assert!(workspace_table);
        drop(migrated);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn migration_seven_creates_bounded_persona_emotion_event_and_projection() {
        let root = unique_root("persona-emotion-state");
        let (_, connection) = super::open_runtime_database(&root).expect("应初始化核心库");
        connection
            .execute_batch(
                "DELETE FROM schema_migrations WHERE version = 7;
                 DROP TABLE persona_state_projection;
                 DROP INDEX persona_state_event_persona_revision;
                 DROP TABLE persona_state_event;",
            )
            .expect("应模拟 migration v6 的运行时库");
        drop(connection);

        let (_, migrated) = super::open_runtime_database(&root).expect("应执行情绪状态迁移");
        for table in ["persona_state_event", "persona_state_projection"] {
            let exists: bool = migrated
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .expect("应检查情绪状态表");
            assert!(exists, "应创建 `{table}`");
        }
        let invalid = migrated.execute(
            "INSERT INTO persona_state_event(
                turn_id, persona_id, conversation_id, source_commit_seq,
                emotion, intensity, reason_code, committed_at
             ) VALUES('turn', 'persona', 'conversation', 1, 'unknown', 101, 'other', 'now')",
            [],
        );
        assert!(invalid.is_err(), "数据库必须拒绝越界或未知情绪状态");
        drop(migrated);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn rejects_runtime_database_from_a_future_schema_version() {
        let root = unique_root("future-version");
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).expect("应创建运行时目录");
        let path = runtime.join("muse.sqlite");
        let connection = Connection::open(&path).expect("应创建测试数据库");
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations(
                    version INTEGER PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE,
                    applied_at TEXT NOT NULL
                );
                INSERT INTO schema_migrations VALUES(99, 'future', 'now');",
            )
            .expect("应写入未来版本");
        drop(connection);

        let error = super::open_runtime_database(&root).expect_err("未来版本必须拒绝降级打开");
        assert!(error.to_string().contains("版本 99 高于"));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn rejects_runtime_database_with_invalid_migration_table_shape() {
        let root = unique_root("invalid-migration-table");
        let (_, connection) = super::open_runtime_database(&root).expect("应初始化统一运行时库");
        connection
            .execute_batch(
                "DROP TABLE schema_migrations;
                 CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY);",
            )
            .expect("应注入 migration 表结构损坏");
        drop(connection);

        let error = super::open_runtime_database(&root).expect_err("损坏 migration 表必须拒绝");
        assert!(error.to_string().contains("schema_migrations"));
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn backup_api_includes_committed_rows_still_in_wal() {
        let root = unique_root("backup-wal");
        let (_, source) = super::open_runtime_database(&root).expect("应初始化源数据库");
        source
            .execute_batch(
                "PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE backup_probe(value TEXT NOT NULL);
                 INSERT INTO backup_probe VALUES('committed-in-wal');",
            )
            .expect("应提交 WAL 测试数据");
        let wal_path = root.join("runtime/muse.sqlite-wal");
        assert!(wal_path.is_file(), "测试必须保留活跃 WAL 文件");

        let backup_directory = root.join("backups");
        std::fs::create_dir_all(&backup_directory).expect("应创建用户选择的备份目录");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&backup_directory, std::fs::Permissions::from_mode(0o755))
                .expect("应设置备份目录测试权限");
        }
        let backup = backup_directory.join("runtime.sqlite");
        let published = super::backup_runtime_database(&root, &backup).expect("应创建一致性备份");
        assert_eq!(published, backup);
        let restored = Connection::open(&backup).expect("应打开备份数据库");
        let value: String = restored
            .query_row("SELECT value FROM backup_probe", [], |row| row.get(0))
            .expect("备份必须包含未 checkpoint 的已提交数据");
        assert_eq!(value, "committed-in-wal");
        let integrity: String = restored
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .expect("应检查备份完整性");
        assert_eq!(integrity, "ok");
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&backup), 0o600, "核心库备份必须保持私有权限");
            assert_eq!(
                unix_mode(&backup_directory),
                0o755,
                "不得修改用户选择的备份父目录权限"
            );
        }
        drop(restored);
        drop(source);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn failed_backup_preserves_last_good_destination_and_cleans_temporary_file() {
        let root = unique_root("backup-failure");
        let runtime = root.join("runtime");
        let backups = root.join("backups");
        std::fs::create_dir_all(&runtime).expect("应创建运行时目录");
        std::fs::create_dir_all(&backups).expect("应创建备份目录");
        std::fs::write(runtime.join("muse.sqlite"), b"not a sqlite database")
            .expect("应写入损坏的源数据库");
        let destination = backups.join("runtime.sqlite");
        std::fs::write(&destination, b"last-good-backup").expect("应写入上一份有效备份占位");

        let error = super::backup_runtime_database(&root, &destination)
            .expect_err("损坏源库不得覆盖上一份备份");

        assert!(
            error.to_string().contains("SQLite"),
            "应返回可诊断的 SQLite 错误：{error}"
        );
        assert_eq!(
            std::fs::read(&destination).expect("应读取上一份备份"),
            b"last-good-backup"
        );
        let temporary_files = std::fs::read_dir(&backups)
            .expect("应读取备份目录")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".runtime.sqlite.")
            })
            .count();
        assert_eq!(temporary_files, 0, "失败后不应遗留临时数据库");
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn migrates_legacy_database_and_removes_obsolete_copy() {
        let root = unique_root("migration");
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).expect("应创建运行时目录");
        let legacy = runtime.join("agent-vp.sqlite");
        let connection = rusqlite::Connection::open(&legacy).expect("应创建 legacy 数据库");
        connection
            .execute_batch("CREATE TABLE state(value TEXT); INSERT INTO state VALUES ('legacy');")
            .expect("应写入 legacy 数据");
        drop(connection);

        let current = super::runtime_database_path(&root);
        assert_eq!(current, runtime.join("muse.sqlite"));
        let migrated = rusqlite::Connection::open(&current).expect("应打开新数据库");
        let value: String = migrated
            .query_row("SELECT value FROM state", [], |row| row.get(0))
            .expect("应读取迁移数据");
        assert_eq!(value, "legacy");
        assert!(!legacy.exists(), "当前数据目录不应继续保留旧数据库名");
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&runtime), 0o700);
            assert_eq!(unix_mode(&current), 0o600);
        }
        drop(migrated);
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn failed_legacy_migration_keeps_source_and_cleans_temporary_database() {
        let root = unique_root("migration-failure");
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).expect("应创建运行时目录");
        let legacy = runtime.join("agent-vp.sqlite");
        std::fs::write(&legacy, b"not a sqlite database").expect("应写入损坏的 legacy 数据库");

        let selected = super::runtime_database_path(&root);

        assert_eq!(selected, legacy);
        assert!(legacy.is_file(), "迁移失败时必须保留 legacy 数据库");
        assert!(
            !runtime.join("muse.sqlite").exists(),
            "迁移失败时不得发布不完整的新数据库"
        );
        let temporary_files = std::fs::read_dir(&runtime)
            .expect("应读取运行时目录")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".muse.sqlite.")
            })
            .count();
        assert_eq!(temporary_files, 0, "迁移失败后不应遗留临时数据库");
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&runtime), 0o700);
            assert_eq!(unix_mode(&legacy), 0o600);
        }
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }

    #[test]
    fn keeps_legacy_database_when_published_database_is_corrupted() {
        let root = unique_root("corruption");
        let runtime = root.join("runtime");
        std::fs::create_dir_all(root.join(".muse-migrations")).expect("应创建迁移标记目录");
        std::fs::create_dir_all(&runtime).expect("应创建运行时目录");
        std::fs::write(
            root.join(".muse-migrations/legacy-workspace-v1.json"),
            b"{}",
        )
        .expect("应写入迁移标记");
        let legacy = runtime.join("agent-vp.sqlite");
        let connection = rusqlite::Connection::open(&legacy).expect("应创建 legacy 数据库");
        connection
            .execute_batch("CREATE TABLE state(value TEXT); INSERT INTO state VALUES ('legacy');")
            .expect("应写入 legacy 数据");
        drop(connection);
        std::fs::write(runtime.join("muse.sqlite"), b"not a sqlite database")
            .expect("应写入损坏的新数据库");

        let selected = super::runtime_database_path(&root);

        assert_eq!(selected, legacy);
        assert!(legacy.is_file(), "校验失败时必须保留可恢复的旧数据库");
        std::fs::remove_dir_all(root).expect("应清理测试目录");
    }
}
