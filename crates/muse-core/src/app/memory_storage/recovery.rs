//! 删除权威重放、FTS 投影重建与 WAL 收口。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::Utc;
use rusqlite::{Connection, TransactionBehavior, params};

use super::authority::{AuthorityAnchor, open_authority_for_anchor};
use super::repository::normalize_search_text;
use crate::app::storage::RuntimeStorageError;
use crate::domain::memory::{
    MemoryDeletionSubject, MemoryError, MemoryErrorCode, MemoryId, MemoryPersonaScope,
};

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
    let guard = authority.begin_guard().map_err(recovery_storage_error)?;
    let events = guard
        .pending_events(anchor.last_applied_revision)
        .map_err(recovery_storage_error)?;
    if events.is_empty() {
        guard.finish().map_err(recovery_storage_error)?;
        return Ok(());
    }

    connection
        .pragma_update(None, "secure_delete", "ON")
        .map_err(RuntimeStorageError::Sqlite)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(RuntimeStorageError::Sqlite)?;
    let max_revision = guard.max_revision().map_err(recovery_storage_error)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(RuntimeStorageError::Sqlite)?;
    for event in &events {
        let memory_ids = memory_ids_for_subjects(&transaction, &event.persona_id, &event.subjects)
            .map_err(recovery_storage_error)?;
        delete_memory_ids(&transaction, &event.persona_id, &memory_ids)
            .map_err(recovery_storage_error)?;
    }
    rebuild_search_projection(&transaction).map_err(recovery_storage_error)?;
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
    transaction.commit().map_err(RuntimeStorageError::Sqlite)?;
    checkpoint_runtime_memory(connection).map_err(recovery_storage_error)?;

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
    guard.finish_durable().map_err(recovery_storage_error)?;
    Ok(())
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
) -> Result<(BTreeSet<MemoryDeletionSubject>, u64), MemoryError> {
    let persona_id = scope.persona_id();
    let mut subjects = BTreeSet::new();
    if let Some(memory_id) = memory_id {
        subjects.insert(MemoryDeletionSubject::Memory {
            persona_id: persona_id.to_string(),
            memory_id: memory_id.clone(),
        });
    } else {
        let mut statement = connection
            .prepare(
                "SELECT memory_id
                 FROM memory_entry
                 WHERE persona_id = ?1
                 ORDER BY memory_id",
            )
            .map_err(repository_unavailable)?;
        let rows = statement
            .query_map([persona_id], |row| row.get::<_, String>(0))
            .map_err(repository_unavailable)?;
        for row in rows {
            subjects.insert(MemoryDeletionSubject::Memory {
                persona_id: persona_id.to_string(),
                memory_id: MemoryId(row.map_err(repository_unavailable)?),
            });
        }
    }

    // Derivation 按彻底遗忘语义扩张同 Persona 的事实集合。每发现新 ID，
    // 必须把该事实全部历史 Derivation 与 SourceTurn 纳入权威，再继续扩张，
    // 直至不动点；否则恢复到 sibling 的旧历史形态时会绕过 tombstone。
    let mut expanded_memory_ids = BTreeSet::new();
    loop {
        let matched_memory_ids = memory_ids_for_subjects(connection, persona_id, &subjects)?;
        let pending = matched_memory_ids
            .difference(&expanded_memory_ids)
            .cloned()
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        for memory_id in pending {
            append_memory_delete_subjects(connection, persona_id, &memory_id, &mut subjects)?;
            expanded_memory_ids.insert(memory_id);
        }
    }
    Ok((subjects, expanded_memory_ids.len() as u64))
}

fn append_memory_delete_subjects(
    connection: &Connection,
    persona_id: &str,
    memory_id: &str,
    subjects: &mut BTreeSet<MemoryDeletionSubject>,
) -> Result<(), MemoryError> {
    subjects.insert(MemoryDeletionSubject::Memory {
        persona_id: persona_id.to_string(),
        memory_id: MemoryId(memory_id.to_string()),
    });
    let mut revision_statement = connection
        .prepare(
            "SELECT derivation_key
             FROM memory_revision
             WHERE persona_id = ?1 AND memory_id = ?2
             ORDER BY revision_id",
        )
        .map_err(repository_unavailable)?;
    let derivations = revision_statement
        .query_map(params![persona_id, memory_id], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(repository_unavailable)?;
    for derivation in derivations {
        let digest: [u8; 32] = derivation
            .map_err(repository_unavailable)?
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
            "SELECT DISTINCT conversation_id, turn_id
             FROM memory_revision_source
             WHERE persona_id = ?1
               AND memory_id = ?2
               AND conversation_id IS NOT NULL
               AND turn_id IS NOT NULL
             ORDER BY conversation_id, turn_id",
        )
        .map_err(repository_unavailable)?;
    let sources = source_statement
        .query_map(params![persona_id, memory_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(repository_unavailable)?;
    for source in sources {
        let (conversation_id, turn_id) = source.map_err(repository_unavailable)?;
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
) -> Result<BTreeSet<String>, MemoryError> {
    let mut memory_ids = BTreeSet::new();
    for subject in subjects {
        match subject {
            MemoryDeletionSubject::Persona {
                persona_id: subject_persona,
            } => {
                require_persona(persona_id, subject_persona)?;
                collect_memory_ids(
                    connection,
                    "SELECT memory_id FROM memory_entry WHERE persona_id = ?1",
                    persona_id,
                    &mut memory_ids,
                )?;
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
                        "SELECT DISTINCT entry.memory_id
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
                        params![persona_id, derivation_key.as_bytes().as_slice()],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(repository_unavailable)?;
                for row in rows {
                    memory_ids.insert(row.map_err(repository_unavailable)?);
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

pub(crate) fn rebuild_search_projection(connection: &Connection) -> Result<(), MemoryError> {
    let personas = projection_personas(connection)?;
    let mut rows_by_persona = BTreeMap::new();
    for persona_id in &personas {
        let mut statement = connection
            .prepare(
                "SELECT revision.row_id, entry.memory_id, revision.revision_id,
                        revision.content, entry.category
                 FROM memory_entry AS entry
                 JOIN memory_revision AS revision
                   ON revision.persona_id = entry.persona_id
                  AND revision.memory_id = entry.memory_id
                  AND revision.revision_id = entry.current_revision_id
                 WHERE entry.persona_id = ?1
                   AND revision.persona_id = ?1
                   AND entry.state = 'active'
                   AND revision.state = 'current'
                   AND revision.valid_to IS NULL
                 ORDER BY entry.memory_id",
            )
            .map_err(repository_unavailable)?;
        let rows = statement
            .query_map([persona_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(repository_unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(repository_unavailable)?;
        rows_by_persona.insert(persona_id.clone(), rows);
    }

    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS memory_search_projection_insert;
             DROP TRIGGER IF EXISTS memory_search_projection_delete;
             DROP TRIGGER IF EXISTS memory_search_projection_update;
             DROP TABLE IF EXISTS memory_fts;",
        )
        .map_err(repository_unavailable)?;
    for persona_id in &personas {
        connection
            .execute(
                "DELETE FROM memory_search_projection WHERE persona_id = ?1",
                [persona_id],
            )
            .map_err(repository_unavailable)?;
    }
    for (persona_id, rows) in rows_by_persona {
        for (row_id, memory_id, revision_id, content, category) in rows {
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

fn projection_personas(connection: &Connection) -> Result<BTreeSet<String>, MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT persona_id FROM memory_entry
             UNION
             SELECT persona_id FROM memory_search_projection
             ORDER BY persona_id",
        )
        .map_err(repository_unavailable)?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(repository_unavailable)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(repository_unavailable)
}

fn collect_memory_ids(
    connection: &Connection,
    sql: &str,
    persona_id: &str,
    target: &mut BTreeSet<String>,
) -> Result<(), MemoryError> {
    let mut statement = connection.prepare(sql).map_err(repository_unavailable)?;
    let rows = statement
        .query_map([persona_id], |row| row.get::<_, String>(0))
        .map_err(repository_unavailable)?;
    for row in rows {
        target.insert(row.map_err(repository_unavailable)?);
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

fn recovery_storage_error(_error: MemoryError) -> RuntimeStorageError {
    RuntimeStorageError::Integrity(
        "记忆删除权威无法安全重放，已拒绝开放 runtime 数据库".to_string(),
    )
}

fn repository_unavailable<T>(_error: T) -> MemoryError {
    MemoryError::new(MemoryErrorCode::RepositoryUnavailable)
}
