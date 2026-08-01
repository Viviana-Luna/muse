//! 删除权威重放、FTS 投影重建与 WAL 收口。

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::authority::{AuthorityAnchor, open_authority_for_anchor};
use super::repository::normalize_search_text;
use crate::app::storage::RuntimeStorageError;
use crate::domain::memory::{
    MAX_MEMORY_CONTENT_BYTES, MAX_MEMORY_DELETION_SUBJECTS, MemoryDeletionSubject, MemoryError,
    MemoryErrorCode, MemoryId, MemoryPersonaScope,
};

const MAX_RECOVERY_ROWS: usize = 512;
const MAX_RECOVERY_SUBJECTS: usize = MAX_MEMORY_DELETION_SUBJECTS;
const MAX_RECOVERY_MATERIALIZED_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECOVERY_TEXT_FIELD_BYTES: usize = 1024;
const MAX_RECOVERY_PROJECTION_ROW_BYTES: usize =
    MAX_MEMORY_CONTENT_BYTES + MAX_RECOVERY_TEXT_FIELD_BYTES * 4;
const RECOVERY_REBUILD_BATCH_SIZE: usize = 64;
const RECOVERY_SQLITE_PROGRESS_INTERVAL_OPS: i32 = 1_000;
const MAX_RECOVERY_SQLITE_PROGRESS_CALLBACKS: usize = 20_000;

pub(crate) struct RecoveryReadBudget {
    rows: usize,
    materialized_bytes: usize,
}

impl RecoveryReadBudget {
    pub(crate) const fn new() -> Self {
        Self {
            rows: 0,
            materialized_bytes: 0,
        }
    }

    fn consume_row(
        &mut self,
        field_bytes: &[usize],
        maximum_row_bytes: usize,
    ) -> Result<(), MemoryError> {
        let row_bytes = field_bytes.iter().try_fold(0_usize, |total, length| {
            total.checked_add(*length).ok_or_else(query_budget_exceeded)
        })?;
        if row_bytes > maximum_row_bytes {
            return Err(query_budget_exceeded());
        }
        let rows = self.rows.checked_add(1).ok_or_else(query_budget_exceeded)?;
        let materialized_bytes = self
            .materialized_bytes
            .checked_add(row_bytes)
            .ok_or_else(query_budget_exceeded)?;
        if rows > MAX_RECOVERY_ROWS || materialized_bytes > MAX_RECOVERY_MATERIALIZED_BYTES {
            return Err(query_budget_exceeded());
        }
        self.rows = rows;
        self.materialized_bytes = materialized_bytes;
        Ok(())
    }
}

pub(crate) fn install_recovery_progress_handler(connection: &Connection) -> Arc<AtomicBool> {
    let callbacks = Arc::new(AtomicUsize::new(0));
    let exhausted = Arc::new(AtomicBool::new(false));
    let callback_count = Arc::clone(&callbacks);
    let callback_exhausted = Arc::clone(&exhausted);
    connection.progress_handler(
        RECOVERY_SQLITE_PROGRESS_INTERVAL_OPS,
        Some(move || {
            let should_interrupt = callback_count.fetch_add(1, Ordering::Relaxed) + 1
                > MAX_RECOVERY_SQLITE_PROGRESS_CALLBACKS;
            if should_interrupt {
                callback_exhausted.store(true, Ordering::Relaxed);
            }
            should_interrupt
        }),
    );
    exhausted
}

pub(crate) fn clear_recovery_progress_handler(connection: &Connection) {
    connection.progress_handler(0, None::<fn() -> bool>);
}

const MEMORY_FTS_SCHEMA: &str = r#"
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
"#;

const MEMORY_SEARCH_NORMALIZATION_VERSION: i64 = 2;
const MEMORY_SEARCH_PROJECTION_META_SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS memory_search_projection_meta (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        normalization_version INTEGER NOT NULL CHECK (normalization_version > 0)
    );
"#;

/// 每次开放 runtime 库前重放独立权威，旧备份不能越过该边界复活正文。
pub(crate) fn reconcile_runtime_memory(
    connection: &mut Connection,
    database_path: &Path,
) -> Result<(), RuntimeStorageError> {
    let anchor = load_anchor(connection).map_err(recovery_storage_error)?;
    let base_dir = database_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            RuntimeStorageError::Integrity("运行时数据库无法定位记忆删除权威恢复域".to_string())
        })?;
    let authority = open_authority_for_anchor(base_dir, &anchor).map_err(recovery_storage_error)?;
    let mut guard = authority.begin_guard().map_err(recovery_storage_error)?;
    let events = guard
        .pending_events(anchor.last_applied_revision)
        .map_err(recovery_storage_error)?;
    let projection_outdated = search_projection_requires_rebuild(connection)?;
    if events.is_empty() && !projection_outdated {
        guard.finish().map_err(recovery_storage_error)?;
        return Ok(());
    }

    connection
        .pragma_update(None, "secure_delete", "ON")
        .map_err(RuntimeStorageError::Sqlite)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(RuntimeStorageError::Sqlite)?;
    let max_revision = if events.is_empty() {
        anchor.last_applied_revision
    } else {
        guard.max_revision().map_err(recovery_storage_error)?
    };
    let exhausted = install_recovery_progress_handler(connection);
    let mut budget = RecoveryReadBudget::new();
    let recovery_result = (|| {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RuntimeStorageError::Sqlite)?;
        for event in &events {
            let memory_ids = memory_ids_for_subjects(
                &transaction,
                &event.persona_id,
                &event.subjects,
                &mut budget,
            )
            .map_err(recovery_storage_error)?;
            delete_memory_ids(&transaction, &event.persona_id, &memory_ids)
                .map_err(recovery_storage_error)?;
        }
        // 删除重放与规范化版本升级共用同一原子投影重建；即使没有待恢复删除，
        // 旧 NFKC 前投影也必须在开放读取前完成升级。
        rebuild_search_projection(&transaction, &mut budget).map_err(recovery_storage_error)?;
        if !events.is_empty() {
            let updated = transaction
                .execute(
                    "UPDATE memory_authority_anchor
                     SET last_applied_revision = ?1
                     WHERE singleton = 1
                       AND authority_id = ?2
                       AND initialized_at = ?3
                       AND last_applied_revision <= ?1",
                    params![max_revision, anchor.authority_id, anchor.initialized_at],
                )
                .map_err(RuntimeStorageError::Sqlite)?;
            if updated != 1 {
                return Err(RuntimeStorageError::Integrity(
                    "记忆删除权威 anchor 在恢复期间发生冲突".to_string(),
                ));
            }
        }
        transaction.commit().map_err(RuntimeStorageError::Sqlite)
    })();
    clear_recovery_progress_handler(connection);
    if exhausted.load(Ordering::Relaxed) {
        return Err(recovery_storage_error(query_budget_exceeded()));
    }
    recovery_result?;
    checkpoint_runtime_memory(connection).map_err(recovery_storage_error)?;
    // FULL 只覆盖恢复事务与随后的 durable checkpoint；正式连接仍回到统一的
    // NORMAL 运行参数，避免一次投影升级永久改变调用方连接语义。
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(RuntimeStorageError::Sqlite)?;

    let completed_at = Utc::now().to_rfc3339();
    for event in &events {
        if event.cleanup_completed_at.is_some() {
            continue;
        }
        let Some(count) = event.target_memory_count else {
            continue;
        };
        guard
            .mark_cleanup_completed(&event.persona_id, &event.deletion_id, count, &completed_at)
            .map_err(recovery_storage_error)?;
    }
    if events.is_empty() {
        guard.finish().map_err(recovery_storage_error)?;
    } else {
        guard.finish_durable().map_err(recovery_storage_error)?;
    }
    Ok(())
}

fn search_projection_requires_rebuild(
    connection: &Connection,
) -> Result<bool, RuntimeStorageError> {
    let metadata_exists = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'memory_search_projection_meta'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(RuntimeStorageError::Sqlite)?;
    if !metadata_exists {
        return Ok(true);
    }
    let version = connection
        .query_row(
            "SELECT normalization_version
             FROM memory_search_projection_meta
             WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(RuntimeStorageError::Sqlite)?;
    match version {
        Some(version) if version == MEMORY_SEARCH_NORMALIZATION_VERSION => Ok(false),
        Some(version) if version > MEMORY_SEARCH_NORMALIZATION_VERSION => {
            Err(RuntimeStorageError::Integrity(format!(
                "记忆检索投影规范化版本 {version} 高于当前支持的 {MEMORY_SEARCH_NORMALIZATION_VERSION}"
            )))
        }
        _ => Ok(true),
    }
}

pub(crate) fn load_anchor(connection: &Connection) -> Result<AuthorityAnchor, MemoryError> {
    connection
        .query_row(
            "SELECT authority_id, initialized_at, last_applied_revision
             FROM memory_authority_anchor
             WHERE singleton = 1",
            [],
            |row| {
                Ok(AuthorityAnchor {
                    authority_id: row.get(0)?,
                    initialized_at: row.get(1)?,
                    last_applied_revision: row.get(2)?,
                })
            },
        )
        .map_err(repository_unavailable)
}

pub(crate) fn collect_delete_subjects(
    connection: &Connection,
    scope: &MemoryPersonaScope,
    memory_id: Option<&MemoryId>,
    budget: &mut RecoveryReadBudget,
) -> Result<(BTreeSet<MemoryDeletionSubject>, u64), MemoryError> {
    let persona_id = scope.persona_id();
    authorize_persona_memory_entries(connection, persona_id)?;
    let mut subjects = BTreeSet::new();
    if let Some(memory_id) = memory_id {
        subjects.insert(MemoryDeletionSubject::Memory {
            persona_id: persona_id.to_string(),
            memory_id: memory_id.clone(),
        });
    } else {
        let mut statement = connection
            .prepare(
                "SELECT CASE
                            WHEN typeof(memory_id) = 'text'
                             AND length(CAST(memory_id AS BLOB)) <= ?2
                            THEN memory_id
                        END,
                        typeof(memory_id) = 'text',
                        length(CAST(memory_id AS BLOB))
                 FROM memory_entry
                 WHERE persona_id = ?1
                 ORDER BY memory_id",
            )
            .map_err(repository_unavailable)?;
        let rows = statement
            .query_map(
                params![persona_id, MAX_RECOVERY_TEXT_FIELD_BYTES as i64],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .map_err(repository_unavailable)?;
        for row in rows {
            let (value, is_text, byte_length) = row.map_err(repository_unavailable)?;
            let (memory_id, memory_id_bytes) =
                required_bounded_text(value, is_text, byte_length, MAX_RECOVERY_TEXT_FIELD_BYTES)?;
            budget.consume_row(&[memory_id_bytes], MAX_RECOVERY_TEXT_FIELD_BYTES)?;
            subjects.insert(MemoryDeletionSubject::Memory {
                persona_id: persona_id.to_string(),
                memory_id: MemoryId(memory_id),
            });
        }
    }

    // Derivation 按彻底遗忘语义扩张同 Persona 的事实集合。每发现新 ID，
    // 必须把该事实全部历史 Derivation 与 SourceTurn 纳入权威，再继续扩张，
    // 直至不动点；否则恢复到 sibling 的旧历史形态时会绕过 tombstone。
    let mut expanded_memory_ids = BTreeSet::new();
    loop {
        let matched_memory_ids =
            memory_ids_for_subjects(connection, persona_id, &subjects, budget)?;
        let pending = matched_memory_ids
            .difference(&expanded_memory_ids)
            .cloned()
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        for memory_id in pending {
            append_memory_delete_subjects(
                connection,
                persona_id,
                &memory_id,
                &mut subjects,
                budget,
            )?;
            expanded_memory_ids.insert(memory_id);
            if subjects.len() > MAX_RECOVERY_SUBJECTS {
                return Err(query_budget_exceeded());
            }
        }
    }
    Ok((subjects, expanded_memory_ids.len() as u64))
}

fn append_memory_delete_subjects(
    connection: &Connection,
    persona_id: &str,
    memory_id: &str,
    subjects: &mut BTreeSet<MemoryDeletionSubject>,
    budget: &mut RecoveryReadBudget,
) -> Result<(), MemoryError> {
    authorize_revision_sources(connection, persona_id, memory_id)?;
    subjects.insert(MemoryDeletionSubject::Memory {
        persona_id: persona_id.to_string(),
        memory_id: MemoryId(memory_id.to_string()),
    });
    let mut revision_statement = connection
        .prepare(
            "SELECT CASE
                        WHEN typeof(derivation_key) = 'blob'
                         AND length(CAST(derivation_key AS BLOB)) = 32
                        THEN derivation_key
                    END,
                    typeof(derivation_key) = 'blob',
                    length(CAST(derivation_key AS BLOB))
             FROM memory_revision
             WHERE persona_id = ?1 AND memory_id = ?2
             ORDER BY revision_id",
        )
        .map_err(repository_unavailable)?;
    let derivations = revision_statement
        .query_map(params![persona_id, memory_id], |row| {
            Ok((
                row.get::<_, Option<Vec<u8>>>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(repository_unavailable)?;
    for derivation in derivations {
        let (value, is_blob, byte_length) = derivation.map_err(repository_unavailable)?;
        let byte_length = required_bounded_blob_length(is_blob, byte_length, 32)?;
        budget.consume_row(&[byte_length], 32)?;
        let digest: [u8; 32] = value
            .ok_or_else(repository_unavailable_marker)?
            .try_into()
            .map_err(repository_unavailable)?;
        subjects.insert(MemoryDeletionSubject::Derivation {
            persona_id: persona_id.to_string(),
            derivation_key: crate::domain::memory::MemoryDerivationKey::from_digest(digest),
        });
    }
    drop(revision_statement);

    let mut source_statement = connection
        .prepare(
            "SELECT DISTINCT
                    CASE
                        WHEN typeof(conversation_id) = 'text'
                         AND length(CAST(conversation_id AS BLOB)) <= ?3
                        THEN conversation_id
                    END,
                    typeof(conversation_id) = 'text',
                    length(CAST(conversation_id AS BLOB)),
                    CASE
                        WHEN typeof(turn_id) = 'text'
                         AND length(CAST(turn_id AS BLOB)) <= ?3
                        THEN turn_id
                    END,
                    typeof(turn_id) = 'text',
                    length(CAST(turn_id AS BLOB))
             FROM memory_revision_source
             WHERE persona_id = ?1
               AND memory_id = ?2
               AND conversation_id IS NOT NULL
               AND turn_id IS NOT NULL
             ORDER BY conversation_id, turn_id",
        )
        .map_err(repository_unavailable)?;
    let sources = source_statement
        .query_map(
            params![persona_id, memory_id, MAX_RECOVERY_TEXT_FIELD_BYTES as i64],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            },
        )
        .map_err(repository_unavailable)?;
    for source in sources {
        let (
            conversation_id,
            conversation_is_text,
            conversation_bytes,
            turn_id,
            turn_is_text,
            turn_bytes,
        ) = source.map_err(repository_unavailable)?;
        let (conversation_id, conversation_bytes) = required_bounded_text(
            conversation_id,
            conversation_is_text,
            conversation_bytes,
            MAX_RECOVERY_TEXT_FIELD_BYTES,
        )?;
        let (turn_id, turn_bytes) = required_bounded_text(
            turn_id,
            turn_is_text,
            turn_bytes,
            MAX_RECOVERY_TEXT_FIELD_BYTES,
        )?;
        budget.consume_row(
            &[conversation_bytes, turn_bytes],
            MAX_RECOVERY_TEXT_FIELD_BYTES * 2,
        )?;
        subjects.insert(MemoryDeletionSubject::SourceTurn {
            persona_id: persona_id.to_string(),
            conversation_id,
            turn_id,
        });
    }
    Ok(())
}

pub(crate) fn memory_ids_for_subjects(
    connection: &Connection,
    persona_id: &str,
    subjects: &BTreeSet<MemoryDeletionSubject>,
    budget: &mut RecoveryReadBudget,
) -> Result<BTreeSet<String>, MemoryError> {
    if subjects.len() > MAX_RECOVERY_SUBJECTS {
        return Err(query_budget_exceeded());
    }
    let mut memory_ids = BTreeSet::new();
    for subject in subjects {
        match subject {
            MemoryDeletionSubject::Persona {
                persona_id: subject_persona,
            } => {
                require_persona(persona_id, subject_persona)?;
                collect_memory_ids(connection, persona_id, &mut memory_ids, budget)?;
            }
            MemoryDeletionSubject::Memory {
                persona_id: subject_persona,
                memory_id,
            } => {
                require_persona(persona_id, subject_persona)?;
                let exists = connection
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM memory_entry
                            WHERE persona_id = ?1 AND memory_id = ?2
                         )",
                        params![persona_id, memory_id.0],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(repository_unavailable)?;
                if exists {
                    let memory_id_bytes = memory_id.0.len();
                    if memory_id_bytes > MAX_RECOVERY_TEXT_FIELD_BYTES {
                        return Err(query_budget_exceeded());
                    }
                    budget.consume_row(&[memory_id_bytes], MAX_RECOVERY_TEXT_FIELD_BYTES)?;
                    memory_ids.insert(memory_id.0.clone());
                }
            }
            MemoryDeletionSubject::SourceTurn {
                persona_id: subject_persona,
                conversation_id: _,
                turn_id: _,
            } => {
                require_persona(persona_id, subject_persona)?;
                // SourceTurn 只阻止后续重扫与新派生；恢复清理不能反向连带删除
                // 同一 Turn 形成但未被用户选择删除的其他记忆。
            }
            MemoryDeletionSubject::Derivation {
                persona_id: subject_persona,
                derivation_key,
            } => {
                require_persona(persona_id, subject_persona)?;
                let mut statement = connection
                    .prepare(
                        "SELECT DISTINCT
                                CASE
                                    WHEN typeof(entry.memory_id) = 'text'
                                     AND length(CAST(entry.memory_id AS BLOB)) <= ?3
                                    THEN entry.memory_id
                                END,
                                typeof(entry.memory_id) = 'text',
                                length(CAST(entry.memory_id AS BLOB))
                         FROM memory_entry AS entry
                         JOIN memory_revision AS revision
                           ON revision.persona_id = entry.persona_id
                          AND revision.memory_id = entry.memory_id
                         WHERE entry.persona_id = ?1
                           AND revision.persona_id = ?1
                           AND revision.derivation_key = ?2",
                    )
                    .map_err(repository_unavailable)?;
                let rows = statement
                    .query_map(
                        params![
                            persona_id,
                            derivation_key.as_bytes().as_slice(),
                            MAX_RECOVERY_TEXT_FIELD_BYTES as i64
                        ],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, bool>(1)?,
                                row.get::<_, Option<i64>>(2)?,
                            ))
                        },
                    )
                    .map_err(repository_unavailable)?;
                for row in rows {
                    let (value, is_text, byte_length) = row.map_err(repository_unavailable)?;
                    let (memory_id, memory_id_bytes) = required_bounded_text(
                        value,
                        is_text,
                        byte_length,
                        MAX_RECOVERY_TEXT_FIELD_BYTES,
                    )?;
                    budget.consume_row(&[memory_id_bytes], MAX_RECOVERY_TEXT_FIELD_BYTES)?;
                    memory_ids.insert(memory_id);
                }
            }
        }
    }
    Ok(memory_ids)
}

pub(crate) fn delete_memory_ids(
    connection: &Connection,
    persona_id: &str,
    memory_ids: &BTreeSet<String>,
) -> Result<(), MemoryError> {
    for memory_id in memory_ids {
        connection
            .execute(
                "DELETE FROM memory_entry
                 WHERE persona_id = ?1 AND memory_id = ?2",
                params![persona_id, memory_id],
            )
            .map_err(repository_unavailable)?;
    }
    Ok(())
}

fn authorize_persona_memory_entries(
    connection: &Connection,
    persona_id: &str,
) -> Result<(), MemoryError> {
    let (row_count, oversized, invalid): (i64, bool, bool) = connection
        .query_row(
            "SELECT
                COUNT(*),
                EXISTS(
                    SELECT 1 FROM memory_entry
                    WHERE persona_id = ?1
                      AND (
                          length(CAST(persona_id AS BLOB)) > ?2
                          OR length(CAST(memory_id AS BLOB)) > ?2
                          OR length(CAST(category AS BLOB)) > ?2
                          OR length(CAST(current_revision_id AS BLOB)) > ?2
                          OR length(CAST(importance AS BLOB)) > ?2
                          OR length(CAST(freshness_at AS BLOB)) > ?2
                          OR length(CAST(created_at AS BLOB)) > ?2
                          OR length(CAST(state AS BLOB)) > ?2
                      )
                    LIMIT 1
                ),
                EXISTS(
                    SELECT 1 FROM memory_entry
                    WHERE persona_id = ?1
                      AND (
                          typeof(persona_id) <> 'text'
                          OR typeof(memory_id) <> 'text'
                          OR typeof(category) <> 'text'
                          OR typeof(current_revision_id) <> 'text'
                          OR typeof(importance) <> 'text'
                          OR typeof(freshness_at) <> 'text'
                          OR typeof(created_at) <> 'text'
                          OR typeof(state) <> 'text'
                          OR category NOT IN (
                              'user_fact', 'user_preference', 'shared_experience',
                              'commitment', 'story_state'
                          )
                          OR importance NOT IN ('low', 'normal', 'high')
                          OR state NOT IN ('active', 'deleted')
                      )
                    LIMIT 1
                )
             FROM memory_entry
             WHERE persona_id = ?1",
            params![persona_id, MAX_RECOVERY_TEXT_FIELD_BYTES as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(repository_unavailable)?;
    let row_count = usize::try_from(row_count).map_err(repository_unavailable)?;
    if row_count > MAX_RECOVERY_ROWS || oversized {
        return Err(query_budget_exceeded());
    }
    if invalid {
        return Err(repository_unavailable_marker());
    }
    Ok(())
}

fn authorize_revision_sources(
    connection: &Connection,
    persona_id: &str,
    memory_id: &str,
) -> Result<(), MemoryError> {
    let (row_count, oversized, invalid): (i64, bool, bool) = connection
        .query_row(
            "SELECT
                COUNT(*),
                EXISTS(
                    SELECT 1 FROM memory_revision_source
                    WHERE persona_id = ?1 AND memory_id = ?2
                      AND (
                          length(CAST(persona_id AS BLOB)) > ?3
                          OR length(CAST(memory_id AS BLOB)) > ?3
                          OR length(CAST(revision_id AS BLOB)) > ?3
                          OR length(CAST(source_kind AS BLOB)) > ?3
                          OR length(CAST(conversation_id AS BLOB)) > ?3
                          OR length(CAST(turn_id AS BLOB)) > ?3
                          OR length(CAST(action_id AS BLOB)) > ?3
                          OR length(CAST(authorized_at AS BLOB)) > ?3
                      )
                    LIMIT 1
                ),
                EXISTS(
                    SELECT 1 FROM memory_revision_source
                    WHERE persona_id = ?1 AND memory_id = ?2
                      AND (
                          typeof(persona_id) <> 'text'
                          OR typeof(memory_id) <> 'text'
                          OR typeof(revision_id) <> 'text'
                          OR typeof(source_ordinal) <> 'integer'
                          OR typeof(source_kind) <> 'text'
                          OR source_kind NOT IN (
                              'direct_user_message', 'user_confirmation',
                              'deterministic_local_event', 'persona_management'
                          )
                          OR (
                              source_kind IN (
                                  'direct_user_message', 'user_confirmation',
                                  'deterministic_local_event'
                              )
                              AND (
                                  typeof(conversation_id) <> 'text'
                                  OR typeof(turn_id) <> 'text'
                                  OR action_id IS NOT NULL
                                  OR authorized_at IS NOT NULL
                              )
                          )
                          OR (
                              source_kind = 'persona_management'
                              AND (
                                  conversation_id IS NOT NULL
                                  OR turn_id IS NOT NULL
                                  OR typeof(action_id) <> 'text'
                                  OR typeof(authorized_at) <> 'text'
                              )
                          )
                      )
                    LIMIT 1
                )
             FROM memory_revision_source
             WHERE persona_id = ?1 AND memory_id = ?2",
            params![persona_id, memory_id, MAX_RECOVERY_TEXT_FIELD_BYTES as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(repository_unavailable)?;
    let row_count = usize::try_from(row_count).map_err(repository_unavailable)?;
    if row_count > MAX_RECOVERY_ROWS || oversized {
        return Err(query_budget_exceeded());
    }
    if invalid {
        return Err(repository_unavailable_marker());
    }
    Ok(())
}

fn authorize_projection_source(connection: &Connection) -> Result<(), MemoryError> {
    let (row_count, oversized, invalid, active_count, joined_count): (i64, bool, bool, i64, i64) =
        connection
            .query_row(
                "SELECT
                (SELECT COUNT(*) FROM memory_entry),
                EXISTS(
                    SELECT 1 FROM memory_entry
                    WHERE length(CAST(persona_id AS BLOB)) > ?1
                       OR length(CAST(memory_id AS BLOB)) > ?1
                       OR length(CAST(category AS BLOB)) > ?1
                       OR length(CAST(current_revision_id AS BLOB)) > ?1
                       OR length(CAST(importance AS BLOB)) > ?1
                       OR length(CAST(freshness_at AS BLOB)) > ?1
                       OR length(CAST(created_at AS BLOB)) > ?1
                       OR length(CAST(state AS BLOB)) > ?1
                    LIMIT 1
                ),
                EXISTS(
                    SELECT 1 FROM memory_entry
                    WHERE typeof(persona_id) <> 'text'
                       OR typeof(memory_id) <> 'text'
                       OR typeof(category) <> 'text'
                       OR typeof(current_revision_id) <> 'text'
                       OR typeof(importance) <> 'text'
                       OR typeof(freshness_at) <> 'text'
                       OR typeof(created_at) <> 'text'
                       OR typeof(state) <> 'text'
                       OR category NOT IN (
                           'user_fact', 'user_preference', 'shared_experience',
                           'commitment', 'story_state'
                       )
                       OR importance NOT IN ('low', 'normal', 'high')
                       OR state NOT IN ('active', 'deleted')
                    LIMIT 1
                ),
                (SELECT COUNT(*) FROM memory_entry WHERE state = 'active'),
                (
                    SELECT COUNT(*)
                    FROM memory_entry AS entry
                    JOIN memory_revision AS revision
                      ON revision.persona_id = entry.persona_id
                     AND revision.memory_id = entry.memory_id
                     AND revision.revision_id = entry.current_revision_id
                    WHERE entry.state = 'active'
                      AND revision.state = 'current'
                      AND revision.valid_to IS NULL
                )",
                [MAX_RECOVERY_TEXT_FIELD_BYTES as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(repository_unavailable)?;
    let row_count = usize::try_from(row_count).map_err(repository_unavailable)?;
    if row_count > MAX_RECOVERY_ROWS || oversized {
        return Err(query_budget_exceeded());
    }
    if invalid || active_count != joined_count {
        return Err(repository_unavailable_marker());
    }
    Ok(())
}

pub(crate) fn rebuild_search_projection(
    connection: &Connection,
    budget: &mut RecoveryReadBudget,
) -> Result<(), MemoryError> {
    authorize_projection_source(connection)?;
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS memory_search_projection_insert;
             DROP TRIGGER IF EXISTS memory_search_projection_delete;
             DROP TRIGGER IF EXISTS memory_search_projection_update;
             DROP TABLE IF EXISTS memory_fts;",
        )
        .map_err(repository_unavailable)?;
    connection
        .execute("DELETE FROM memory_search_projection", [])
        .map_err(repository_unavailable)?;

    let mut after_persona: Option<String> = None;
    let mut after_memory: Option<String> = None;
    loop {
        let mut statement = connection
            .prepare(
                "SELECT revision.row_id,
                        CASE
                            WHEN typeof(entry.persona_id) = 'text'
                             AND length(CAST(entry.persona_id AS BLOB)) <= ?3
                            THEN entry.persona_id
                        END,
                        typeof(entry.persona_id) = 'text',
                        length(CAST(entry.persona_id AS BLOB)),
                        CASE
                            WHEN typeof(entry.memory_id) = 'text'
                             AND length(CAST(entry.memory_id AS BLOB)) <= ?3
                            THEN entry.memory_id
                        END,
                        typeof(entry.memory_id) = 'text',
                        length(CAST(entry.memory_id AS BLOB)),
                        CASE
                            WHEN typeof(revision.revision_id) = 'text'
                             AND length(CAST(revision.revision_id AS BLOB)) <= ?3
                            THEN revision.revision_id
                        END,
                        typeof(revision.revision_id) = 'text',
                        length(CAST(revision.revision_id AS BLOB)),
                        CASE
                            WHEN typeof(revision.content) = 'text'
                             AND length(CAST(revision.content AS BLOB)) <= ?4
                            THEN revision.content
                        END,
                        typeof(revision.content) = 'text',
                        length(CAST(revision.content AS BLOB)),
                        CASE
                            WHEN typeof(entry.category) = 'text'
                             AND length(CAST(entry.category AS BLOB)) <= ?3
                            THEN entry.category
                        END,
                        typeof(entry.category) = 'text',
                        length(CAST(entry.category AS BLOB))
                 FROM memory_entry AS entry
                 JOIN memory_revision AS revision
                   ON revision.persona_id = entry.persona_id
                  AND revision.memory_id = entry.memory_id
                  AND revision.revision_id = entry.current_revision_id
                 WHERE entry.state = 'active'
                   AND revision.state = 'current'
                   AND revision.valid_to IS NULL
                   AND (
                       ?1 IS NULL
                       OR entry.persona_id > ?1
                       OR (entry.persona_id = ?1 AND entry.memory_id > ?2)
                   )
                 ORDER BY entry.persona_id, entry.memory_id
                 LIMIT ?5",
            )
            .map_err(repository_unavailable)?;
        let rows = statement
            .query_map(
                params![
                    after_persona.as_deref(),
                    after_memory.as_deref(),
                    MAX_RECOVERY_TEXT_FIELD_BYTES as i64,
                    MAX_MEMORY_CONTENT_BYTES as i64,
                    RECOVERY_REBUILD_BATCH_SIZE as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        (
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, bool>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                        ),
                        (
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, bool>(5)?,
                            row.get::<_, Option<i64>>(6)?,
                        ),
                        (
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, bool>(8)?,
                            row.get::<_, Option<i64>>(9)?,
                        ),
                        (
                            row.get::<_, Option<String>>(10)?,
                            row.get::<_, bool>(11)?,
                            row.get::<_, Option<i64>>(12)?,
                        ),
                        (
                            row.get::<_, Option<String>>(13)?,
                            row.get::<_, bool>(14)?,
                            row.get::<_, Option<i64>>(15)?,
                        ),
                    ))
                },
            )
            .map_err(repository_unavailable)?;
        let mut batch = Vec::with_capacity(RECOVERY_REBUILD_BATCH_SIZE);
        for row in rows {
            let (row_id, persona, memory, revision, content, category) =
                row.map_err(repository_unavailable)?;
            let (persona_id, persona_bytes) = required_bounded_text(
                persona.0,
                persona.1,
                persona.2,
                MAX_RECOVERY_TEXT_FIELD_BYTES,
            )?;
            let (memory_id, memory_bytes) =
                required_bounded_text(memory.0, memory.1, memory.2, MAX_RECOVERY_TEXT_FIELD_BYTES)?;
            let (revision_id, revision_bytes) = required_bounded_text(
                revision.0,
                revision.1,
                revision.2,
                MAX_RECOVERY_TEXT_FIELD_BYTES,
            )?;
            let (content, content_bytes) =
                required_bounded_text(content.0, content.1, content.2, MAX_MEMORY_CONTENT_BYTES)?;
            let (category, category_bytes) = required_bounded_text(
                category.0,
                category.1,
                category.2,
                MAX_RECOVERY_TEXT_FIELD_BYTES,
            )?;
            budget.consume_row(
                &[
                    persona_bytes,
                    memory_bytes,
                    revision_bytes,
                    content_bytes,
                    category_bytes,
                ],
                MAX_RECOVERY_PROJECTION_ROW_BYTES,
            )?;
            batch.push((
                row_id,
                persona_id,
                memory_id,
                revision_id,
                content,
                category,
            ));
        }
        drop(statement);
        if batch.is_empty() {
            break;
        }
        let last = batch
            .last()
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
        after_persona = Some(last.1.clone());
        after_memory = Some(last.2.clone());
        for (row_id, persona_id, memory_id, revision_id, content, category) in batch {
            connection
                .execute(
                    "INSERT INTO memory_search_projection(
                        row_id, persona_id, memory_id, revision_id, content, category
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        row_id,
                        persona_id,
                        memory_id,
                        revision_id,
                        normalize_search_text(&content),
                        category
                    ],
                )
                .map_err(repository_unavailable)?;
        }
    }
    connection
        .execute_batch(MEMORY_FTS_SCHEMA)
        .map_err(repository_unavailable)?;
    connection
        .execute("INSERT INTO memory_fts(memory_fts) VALUES('rebuild')", [])
        .map_err(repository_unavailable)?;
    connection
        .execute_batch(MEMORY_SEARCH_PROJECTION_META_SCHEMA)
        .map_err(repository_unavailable)?;
    connection
        .execute(
            "INSERT INTO memory_search_projection_meta(singleton, normalization_version)
             VALUES(1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET normalization_version = excluded.normalization_version",
            [MEMORY_SEARCH_NORMALIZATION_VERSION],
        )
        .map_err(repository_unavailable)?;
    Ok(())
}

pub(crate) fn checkpoint_runtime_memory(connection: &Connection) -> Result<(), MemoryError> {
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(repository_unavailable)?;
    if busy != 0 || log_frames != 0 || checkpointed_frames != 0 {
        return Err(MemoryError::new(MemoryErrorCode::DeletionIncomplete));
    }
    Ok(())
}

fn collect_memory_ids(
    connection: &Connection,
    persona_id: &str,
    target: &mut BTreeSet<String>,
    budget: &mut RecoveryReadBudget,
) -> Result<(), MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT CASE
                        WHEN typeof(memory_id) = 'text'
                         AND length(CAST(memory_id AS BLOB)) <= ?2
                        THEN memory_id
                    END,
                    typeof(memory_id) = 'text',
                    length(CAST(memory_id AS BLOB))
             FROM memory_entry
             WHERE persona_id = ?1
             ORDER BY memory_id",
        )
        .map_err(repository_unavailable)?;
    let rows = statement
        .query_map(
            params![persona_id, MAX_RECOVERY_TEXT_FIELD_BYTES as i64],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .map_err(repository_unavailable)?;
    for row in rows {
        let (value, is_text, byte_length) = row.map_err(repository_unavailable)?;
        let (memory_id, memory_id_bytes) =
            required_bounded_text(value, is_text, byte_length, MAX_RECOVERY_TEXT_FIELD_BYTES)?;
        budget.consume_row(&[memory_id_bytes], MAX_RECOVERY_TEXT_FIELD_BYTES)?;
        target.insert(memory_id);
    }
    Ok(())
}

fn require_persona(expected: &str, actual: &str) -> Result<(), MemoryError> {
    if expected == actual {
        Ok(())
    } else {
        Err(MemoryError::new(MemoryErrorCode::PersonaScopeMismatch))
    }
}

fn required_bounded_text(
    value: Option<String>,
    is_text: bool,
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<(String, usize), MemoryError> {
    let byte_length = required_bounded_length(byte_length, maximum_bytes)?;
    if !is_text {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    let value = value.ok_or_else(repository_unavailable_marker)?;
    if value.len() != byte_length {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    Ok((value, byte_length))
}

fn required_bounded_blob_length(
    is_blob: bool,
    byte_length: Option<i64>,
    expected_bytes: usize,
) -> Result<usize, MemoryError> {
    let byte_length = required_bounded_length(byte_length, expected_bytes)?;
    if !is_blob || byte_length != expected_bytes {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    Ok(byte_length)
}

fn required_bounded_length(
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<usize, MemoryError> {
    let byte_length = byte_length
        .and_then(|length| usize::try_from(length).ok())
        .ok_or_else(repository_unavailable_marker)?;
    if byte_length > maximum_bytes {
        return Err(query_budget_exceeded());
    }
    Ok(byte_length)
}

fn repository_unavailable_marker() -> MemoryError {
    MemoryError::new(MemoryErrorCode::RepositoryUnavailable)
}

fn query_budget_exceeded() -> MemoryError {
    MemoryError::new(MemoryErrorCode::QueryBudgetExceeded)
}

fn recovery_storage_error(_error: MemoryError) -> RuntimeStorageError {
    RuntimeStorageError::Integrity(
        "记忆删除权威无法安全重放，已拒绝开放 runtime 数据库".to_string(),
    )
}

fn repository_unavailable<T>(_error: T) -> MemoryError {
    repository_unavailable_marker()
}
