//! Persona 长期记忆的 SQLite Repository。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::authority::{
    AuthorityDeleteIntent, AuthorityDeletionEvent, SqliteMemoryDeletionAuthority,
};
use super::recovery::{
    checkpoint_runtime_memory, collect_delete_subjects, delete_memory_ids, memory_ids_for_subjects,
    rebuild_search_projection,
};
use crate::app::storage::{open_initialized_runtime_database, open_runtime_database};
use crate::domain::memory::{
    ConfirmedMemoryDeleteRequest, MemoryBatchCommitReceipt, MemoryCategory, MemoryChangeType,
    MemoryCommitEnvelope, MemoryDeleteParams, MemoryDeleteReceipt, MemoryDeletionAuthority,
    MemoryDeletionAuthorityReceipt, MemoryDeletionCheckRequest, MemoryDeletionDecision,
    MemoryDeletionSubject, MemoryDerivationKey, MemoryEntry, MemoryEntryState, MemoryError,
    MemoryErrorCode, MemoryId, MemoryImportance, MemoryImportanceAdjustment,
    MemoryImportanceAdjustmentReceipt, MemoryManagementContentMutation,
    MemoryManagementContentParams, MemoryMutationReceipt, MemoryMutationReceiptState,
    MemoryMutationTransition, MemoryPersonaScope, MemoryRecord, MemoryRepository, MemoryRevision,
    MemoryRevisionId, MemoryRevisionState, MemorySensitivityPolicy, MemorySourceEvidence,
    MemorySourceKind,
};

/// 所有记忆写入、FTS 投影和幂等收据的唯一 SQLite 实现。
pub struct SqliteMemoryRepository {
    base_dir: PathBuf,
    authority: Arc<SqliteMemoryDeletionAuthority>,
}

impl fmt::Debug for SqliteMemoryRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SqliteMemoryRepository([去敏存储句柄])")
    }
}

impl SqliteMemoryRepository {
    pub fn open(base_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let base_dir = base_dir.as_ref().to_path_buf();
        let (_, connection) = open_runtime_database(&base_dir).map_err(repository_unavailable)?;
        configure_memory_connection(&connection)?;
        drop(connection);
        let authority = Arc::new(SqliteMemoryDeletionAuthority::open(&base_dir)?);
        Ok(Self {
            base_dir,
            authority,
        })
    }

    pub fn deletion_authority(&self) -> &SqliteMemoryDeletionAuthority {
        &self.authority
    }

    /// 从 current revision 重建 Persona 隔离的 trigram 投影。
    pub fn rebuild_search_index(&self) -> Result<(), MemoryError> {
        let authority_guard = self.authority.begin_guard()?;
        let mut connection = self.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(repository_unavailable)?;
        rebuild_search_projection(&transaction)?;
        transaction.commit().map_err(repository_unavailable)?;
        authority_guard.finish()?;
        checkpoint_runtime_memory(&connection)
    }

    /// 提供给后续 Retriever 的 storage-level trigram 候选，不实现 Wave 3 排序或游标。
    pub fn search_current_fts(
        &self,
        scope: &MemoryPersonaScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        if limit == 0 || limit > 100 {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        let normalized = normalize_memory_fts_query(query)?;
        let authority_guard = self.authority.begin_guard()?;
        let connection = self.open_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT projection.memory_id
                 FROM memory_fts
                 JOIN memory_search_projection AS projection
                   ON projection.row_id = memory_fts.rowid
                  AND projection.persona_id = ?2
                 WHERE memory_fts MATCH ?1
                   AND memory_fts.persona_id = ?2
                 ORDER BY bm25(memory_fts), projection.memory_id
                 LIMIT ?3",
            )
            .map_err(repository_unavailable)?;
        let memory_ids = statement
            .query_map(
                params![normalized, scope.persona_id(), limit as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(repository_unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(repository_unavailable)?;
        drop(statement);

        let mut records = Vec::with_capacity(memory_ids.len());
        for memory_id in memory_ids {
            let Some(record) = load_current(&connection, scope, &MemoryId(memory_id))? else {
                continue;
            };
            if record_is_blocked(&authority_guard, &self.authority, scope, &record)? {
                continue;
            }
            records.push(record);
        }
        authority_guard.finish()?;
        Ok(records)
    }

    pub(crate) fn open_connection(&self) -> Result<Connection, MemoryError> {
        let connection =
            open_initialized_runtime_database(&self.base_dir).map_err(repository_unavailable)?;
        configure_memory_connection(&connection)?;
        Ok(connection)
    }
}

impl MemoryRepository for SqliteMemoryRepository {
    fn current(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
    ) -> Result<Option<MemoryRecord>, MemoryError> {
        let authority_guard = self.authority.begin_guard()?;
        let connection = self.open_connection()?;
        let record = load_current(&connection, scope, memory_id)?;
        let result = match record {
            Some(record)
                if record_is_blocked(&authority_guard, &self.authority, scope, &record)? =>
            {
                None
            }
            other => other,
        };
        authority_guard.finish()?;
        Ok(result)
    }

    fn apply_committed_batch(
        &self,
        envelope: &MemoryCommitEnvelope,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryBatchCommitReceipt, MemoryError> {
        let digest = envelope_digest(&self.authority, envelope)?;
        let authority_guard = self.authority.begin_guard()?;
        let mut connection = self.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(repository_unavailable)?;

        if let Some(receipt) = load_batch_receipt(&transaction, envelope, &digest)? {
            transaction.commit().map_err(repository_unavailable)?;
            authority_guard.finish()?;
            return Ok(receipt);
        }
        reject_duplicate_committed_turn(&transaction, envelope)?;

        // 全批次先完成第二门、删除阻断与 revision transition，之后才允许首个正文 SQL。
        let mut current_by_memory = BTreeMap::<String, Option<MemoryRecord>>::new();
        let mut transitions = Vec::with_capacity(envelope.mutations().len());
        for mutation in envelope.mutations() {
            let check = mutation_deletion_check(
                &self.authority,
                envelope.scope(),
                mutation.assigned_memory_id(),
                mutation.params().content(),
                mutation.binding().source(),
            )?;
            require_deletion_allowed(authority_guard.check(&check)?)?;

            let memory_key = mutation.assigned_memory_id().0.clone();
            if !current_by_memory.contains_key(&memory_key) {
                current_by_memory.insert(
                    memory_key.clone(),
                    load_current(
                        &transaction,
                        envelope.scope(),
                        mutation.assigned_memory_id(),
                    )?,
                );
            }
            let current = current_by_memory
                .get(&memory_key)
                .expect("刚写入的 current cache 必须存在");
            let transition = mutation.transition(current.as_ref(), sensitivity)?;
            current_by_memory.insert(
                memory_key,
                Some(MemoryRecord {
                    entry: transition.entry.clone(),
                    current_revision: transition.new_revision.clone(),
                }),
            );
            transitions.push((mutation, transition));
        }

        for (_, transition) in &transitions {
            apply_transition(&transaction, &self.authority, envelope.scope(), transition)?;
        }
        let durable_at = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO memory_committed_batch(
                    persona_id, idempotency_key, conversation_id, turn_id,
                    envelope_digest, durable_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    envelope.scope().persona_id(),
                    envelope.idempotency_key(),
                    envelope.conversation_id(),
                    envelope.turn_id(),
                    digest.as_slice(),
                    durable_at
                ],
            )
            .map_err(repository_unavailable)?;
        let mut receipts = Vec::with_capacity(transitions.len());
        for (ordinal, (mutation, transition)) in transitions.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO memory_committed_operation(
                        persona_id, idempotency_key, operation_ordinal, operation_id,
                        change_type, memory_id, revision_id
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        envelope.scope().persona_id(),
                        envelope.idempotency_key(),
                        ordinal as i64,
                        mutation.binding().operation_id(),
                        change_type_to_str(transition.new_revision.change_type),
                        transition.entry.memory_id.0,
                        transition.new_revision.revision_id.0
                    ],
                )
                .map_err(repository_unavailable)?;
            receipts.push(MemoryMutationReceipt {
                operation: transition.new_revision.change_type,
                memory_id: transition.entry.memory_id.clone(),
                revision_id: transition.new_revision.revision_id.clone(),
                state: MemoryMutationReceiptState::Durable,
            });
        }
        transaction.commit().map_err(repository_unavailable)?;
        authority_guard.finish()?;
        Ok(MemoryBatchCommitReceipt {
            idempotency_key: envelope.idempotency_key().to_string(),
            mutations: receipts,
            durable_at,
        })
    }

    fn apply_management_content_mutation(
        &self,
        mutation: &MemoryManagementContentMutation,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        let scope = mutation.binding().scope();
        let digest = management_content_digest(&self.authority, mutation)?;
        let authority_guard = self.authority.begin_guard()?;
        let mut connection = self.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(repository_unavailable)?;
        if let Some(receipt) = load_management_content_receipt(&transaction, mutation, &digest)? {
            transaction.commit().map_err(repository_unavailable)?;
            authority_guard.finish()?;
            return Ok(receipt);
        }

        let check = mutation_deletion_check(
            &self.authority,
            scope,
            mutation.assigned_memory_id(),
            mutation.params().content(),
            mutation.binding().source(),
        )?;
        require_deletion_allowed(authority_guard.check(&check)?)?;
        let current = load_current(&transaction, scope, mutation.assigned_memory_id())?;
        let transition = mutation.transition(current.as_ref(), sensitivity)?;
        apply_transition(&transaction, &self.authority, scope, &transition)?;
        let durable_at = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO memory_management_operation(
                    persona_id, operation_id, operation_kind, mutation_digest,
                    change_type, memory_id, revision_id, durable_at
                 ) VALUES(?1, ?2, 'content', ?3, ?4, ?5, ?6, ?7)",
                params![
                    scope.persona_id(),
                    mutation.binding().operation_id(),
                    digest.as_slice(),
                    change_type_to_str(transition.new_revision.change_type),
                    transition.entry.memory_id.0,
                    transition.new_revision.revision_id.0,
                    durable_at
                ],
            )
            .map_err(repository_unavailable)?;
        transaction.commit().map_err(repository_unavailable)?;
        authority_guard.finish()?;
        Ok(MemoryMutationReceipt {
            operation: transition.new_revision.change_type,
            memory_id: transition.entry.memory_id,
            revision_id: transition.new_revision.revision_id,
            state: MemoryMutationReceiptState::Durable,
        })
    }

    fn adjust_importance(
        &self,
        adjustment: &MemoryImportanceAdjustment,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError> {
        let scope = adjustment.binding().scope();
        let digest = importance_digest(&self.authority, adjustment);
        let authority_guard = self.authority.begin_guard()?;
        let mut connection = self.open_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(repository_unavailable)?;
        if let Some(receipt) = load_importance_receipt(&transaction, adjustment, &digest)? {
            transaction.commit().map_err(repository_unavailable)?;
            authority_guard.finish()?;
            return Ok(receipt);
        }
        let current = load_current(&transaction, scope, adjustment.memory_id())?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if record_is_blocked(&authority_guard, &self.authority, scope, &current)? {
            return Err(MemoryError::new(MemoryErrorCode::MemoryNotFound));
        }
        let adjusted = adjustment.apply_to(&current)?;
        let changed = transaction
            .execute(
                "UPDATE memory_entry
                 SET importance = ?1, freshness_at = ?2
                 WHERE persona_id = ?3
                   AND memory_id = ?4
                   AND current_revision_id = ?5
                   AND importance = ?6",
                params![
                    importance_to_str(adjusted.importance),
                    adjusted.freshness_at,
                    scope.persona_id(),
                    adjusted.memory_id.0,
                    adjustment.expected_revision_id().0,
                    importance_to_str(adjustment.expected_importance())
                ],
            )
            .map_err(repository_unavailable)?;
        if changed != 1 {
            return Err(MemoryError::new(MemoryErrorCode::RevisionConflict));
        }
        let durable_at = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO memory_management_operation(
                    persona_id, operation_id, operation_kind, mutation_digest,
                    memory_id, previous_importance, importance, durable_at
                 ) VALUES(?1, ?2, 'importance', ?3, ?4, ?5, ?6, ?7)",
                params![
                    scope.persona_id(),
                    adjustment.binding().operation_id(),
                    digest.as_slice(),
                    adjustment.memory_id().0,
                    importance_to_str(adjustment.expected_importance()),
                    importance_to_str(adjustment.importance()),
                    durable_at
                ],
            )
            .map_err(repository_unavailable)?;
        transaction.commit().map_err(repository_unavailable)?;
        authority_guard.finish()?;
        Ok(MemoryImportanceAdjustmentReceipt {
            operation_id: adjustment.binding().operation_id().to_string(),
            memory_id: adjustment.memory_id().clone(),
            previous_importance: adjustment.expected_importance(),
            importance: adjustment.importance(),
            durable_at,
        })
    }

    fn delete_confirmed(
        &self,
        request: &ConfirmedMemoryDeleteRequest,
        authority: &dyn MemoryDeletionAuthority,
    ) -> Result<MemoryDeleteReceipt, MemoryError> {
        request.validate_for_repository()?;
        let scope = request.scope();
        let persona_id = scope.persona_id();
        let deletion_id = request.confirmation().confirmation_id();
        let recorded_at = request.confirmation().confirmed_at();
        let (request_kind, requested_memory_id) = match request.params() {
            MemoryDeleteParams::Memory { memory_id } => ("memory", Some(memory_id.0.as_str())),
            MemoryDeleteParams::PersonaAll => ("persona_all", None),
        };
        let supplied_authority = authority as *const dyn MemoryDeletionAuthority as *const ();
        let canonical_authority =
            self.authority.as_ref() as *const dyn MemoryDeletionAuthority as *const ();
        if !std::ptr::eq(supplied_authority, canonical_authority) {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }

        let confirmation_subject = MemoryDeletionSubject::Derivation {
            persona_id: persona_id.to_string(),
            derivation_key: MemoryDerivationKey::from_digest(
                request.confirmation().intent_digest(),
            ),
        };

        let mut authority_guard = self.authority.begin_guard()?;
        let existing = authority_guard.event_by_deletion_id(deletion_id)?;
        if let Some(event) = &existing {
            if event.persona_id != persona_id {
                return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
            }
            validate_delete_event(
                event,
                recorded_at,
                request_kind,
                requested_memory_id,
                &confirmation_subject,
            )?;
        }

        // authority 首次持锁期间只做主库 autocommit 读取；Repository 的所有记忆
        // 写入也先取 authority，因此 canonical subjects 在该窗口保持稳定。
        // 此处绝不能先开启主库 IMMEDIATE，否则 durable authority COMMIT 后重锁
        // 会与另一进程形成 main -> authority / authority -> main 的 ABBA 环。
        let mut connection = self.open_connection()?;
        let (mut subjects, target_count) = if let Some(event) = &existing {
            let count = match event.target_memory_count {
                Some(count) => count,
                None => {
                    memory_ids_for_subjects(&connection, persona_id, &event.subjects)?.len() as u64
                }
            };
            (event.subjects.clone(), count)
        } else {
            match request.params() {
                MemoryDeleteParams::Memory { memory_id } => {
                    collect_delete_subjects(&connection, scope, Some(memory_id))?
                }
                MemoryDeleteParams::PersonaAll => {
                    collect_delete_subjects(&connection, scope, None)?
                }
            }
        };
        subjects.insert(confirmation_subject.clone());

        // 删除事件、具体 subjects 与 intent 全部由当前 guard 连接一次 durable 发布；
        // 重取 authority IMMEDIATE 成功之后才允许开启主库 IMMEDIATE。
        let authority_receipt = authority_guard.record_delete_intent_durable(
            persona_id,
            deletion_id,
            recorded_at,
            &subjects,
            AuthorityDeleteIntent {
                request_kind,
                requested_memory_id,
                target_memory_count: target_count,
            },
        )?;
        validate_authority_receipt(&authority_receipt, deletion_id, &subjects)?;

        // 固定锁序从这里开始始终为 authority -> main；持有 main 期间不再释放或
        // 重取 authority，commit/relock 窗口中的并发删除因此不会形成锁环。
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(repository_unavailable)?;
        let canonical_event = authority_guard
            .event(persona_id, deletion_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable))?;
        validate_delete_event(
            &canonical_event,
            recorded_at,
            request_kind,
            requested_memory_id,
            &confirmation_subject,
        )?;
        if canonical_event.subjects != subjects
            || canonical_event.target_memory_count != Some(target_count)
        {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }
        if !subjects.is_empty() {
            let check = MemoryDeletionCheckRequest::new(subjects.clone())?;
            match authority_guard.check(&check)? {
                MemoryDeletionDecision::Blocked { matched } if matched == subjects => {}
                _ => {
                    return Err(MemoryError::new(
                        MemoryErrorCode::DeletionAuthorityUnavailable,
                    ));
                }
            }
        }

        let anchor = super::recovery::load_anchor(&transaction)?;
        let mut events = authority_guard.pending_events(anchor.last_applied_revision)?;
        if !events
            .iter()
            .any(|event| event.persona_id == persona_id && event.deletion_id == deletion_id)
        {
            events.push(canonical_event.clone());
            events.sort_by_key(|event| event.authority_revision);
        }
        for event in &events {
            let memory_ids =
                memory_ids_for_subjects(&transaction, &event.persona_id, &event.subjects)?;
            delete_memory_ids(&transaction, &event.persona_id, &memory_ids)?;
        }
        rebuild_search_projection(&transaction)?;
        let max_revision = authority_guard.max_revision()?;
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
            .map_err(repository_unavailable)?;
        if updated != 1 {
            return Err(MemoryError::new(MemoryErrorCode::DeletionIncomplete));
        }
        transaction.commit().map_err(repository_unavailable)?;
        checkpoint_runtime_memory(&connection)?;

        let completed_at = Utc::now().to_rfc3339();
        for event in &events {
            let Some(count) = event.target_memory_count else {
                continue;
            };
            authority_guard.mark_cleanup_completed(
                &event.persona_id,
                &event.deletion_id,
                count,
                &completed_at,
            )?;
        }
        let completed_event = authority_guard
            .event(persona_id, deletion_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionIncomplete))?;
        let completed_at = completed_event
            .cleanup_completed_at
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionIncomplete))?;
        authority_guard.finish_durable()?;
        Ok(MemoryDeleteReceipt {
            deletion_id: deletion_id.to_string(),
            deleted_memory_count: target_count,
            completed_at,
        })
    }
}

fn validate_delete_event(
    event: &AuthorityDeletionEvent,
    recorded_at: &str,
    request_kind: &str,
    requested_memory_id: Option<&str>,
    confirmation_subject: &MemoryDeletionSubject,
) -> Result<(), MemoryError> {
    if event.recorded_at != recorded_at || !event.subjects.contains(confirmation_subject) {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    if let Some(stored_kind) = event.request_kind.as_deref()
        && (stored_kind != request_kind
            || event.requested_memory_id.as_deref() != requested_memory_id)
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    match (request_kind, requested_memory_id) {
        ("memory", Some(memory_id))
            if event.subjects.iter().any(|subject| {
                matches!(
                    subject,
                    MemoryDeletionSubject::Memory {
                        persona_id,
                        memory_id: subject_memory_id,
                    } if persona_id == &event.persona_id && subject_memory_id.0 == memory_id
                )
            }) =>
        {
            Ok(())
        }
        ("persona_all", None)
            if !event
                .subjects
                .iter()
                .any(|subject| matches!(subject, MemoryDeletionSubject::Persona { .. })) =>
        {
            Ok(())
        }
        _ => Err(MemoryError::new(MemoryErrorCode::InvalidRequest)),
    }
}

fn validate_authority_receipt(
    receipt: &MemoryDeletionAuthorityReceipt,
    deletion_id: &str,
    subjects: &BTreeSet<MemoryDeletionSubject>,
) -> Result<(), MemoryError> {
    if receipt.deletion_id != deletion_id
        || &receipt.subjects != subjects
        || receipt.authority_revision.trim().is_empty()
        || chrono::DateTime::parse_from_rfc3339(&receipt.durable_at).is_err()
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    Ok(())
}

/// FTS 查询与投影使用同一个确定性规范化器：仅保留 Unicode 字母数字并转小写。
pub fn normalize_memory_fts_query(query: &str) -> Result<String, MemoryError> {
    let normalized = normalize_search_text(query);
    if normalized.chars().count() < 3 {
        return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
    }
    Ok(normalized)
}

pub(crate) fn normalize_search_text(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

/// 删除派生摘要的 canonicalization，与 FTS 规范化完全分离。
///
/// 安全理由：Derivation tombstone 是“彻底遗忘”的唯一事实边界——同一派生键会
/// 连删同 Persona 内全部同键记忆，并永久阻断未来同键写入。因此这里必须保守，
/// 只合并首尾空白与 Unicode 大小写差异，完整保留语义标点、符号与 emoji；若复用
/// FTS 的 alphanumeric 归一化，“用户喜欢 C++”与“用户喜欢 C”、“血型 A+”与
/// “血型 A”甚至任意两条纯 emoji 都会被误并为同一派生，单删将不可逆连删无关
/// 记忆并阻断其未来重写。FTS 归一化只服务召回，继续独立使用。
pub(crate) fn canonicalize_derivation_content(value: &str) -> String {
    value.trim().chars().flat_map(char::to_lowercase).collect()
}

fn configure_memory_connection(connection: &Connection) -> Result<(), MemoryError> {
    connection
        .pragma_update(None, "secure_delete", "ON")
        .map_err(repository_unavailable)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(repository_unavailable)?;
    Ok(())
}

fn load_current(
    connection: &Connection,
    scope: &MemoryPersonaScope,
    memory_id: &MemoryId,
) -> Result<Option<MemoryRecord>, MemoryError> {
    let row = connection
        .query_row(
            "SELECT
                entry.category, entry.current_revision_id, entry.importance,
                entry.freshness_at, entry.created_at, entry.state,
                revision.content, revision.event_time, revision.recorded_at,
                revision.valid_from, revision.valid_to, revision.change_type,
                revision.change_reason, revision.safety_policy_version, revision.state
             FROM memory_entry AS entry
             JOIN memory_revision AS revision
               ON revision.persona_id = entry.persona_id
              AND revision.memory_id = entry.memory_id
              AND revision.revision_id = entry.current_revision_id
             WHERE entry.persona_id = ?1
               AND entry.memory_id = ?2
               AND entry.state = 'active'
               AND revision.state = 'current'
               AND revision.valid_to IS NULL",
            params![scope.persona_id(), memory_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                ))
            },
        )
        .optional()
        .map_err(repository_unavailable)?;
    let Some((
        category,
        current_revision_id,
        importance,
        freshness_at,
        created_at,
        entry_state,
        content,
        event_time,
        recorded_at,
        valid_from,
        valid_to,
        change_type,
        change_reason,
        safety_policy_version,
        revision_state,
    )) = row
    else {
        return Ok(None);
    };
    let source = load_revision_source(
        connection,
        scope,
        memory_id,
        &MemoryRevisionId(current_revision_id.clone()),
    )?;
    Ok(Some(MemoryRecord {
        entry: MemoryEntry {
            memory_id: memory_id.clone(),
            persona_id: scope.persona_id().to_string(),
            category: parse_category(&category)?,
            current_revision_id: MemoryRevisionId(current_revision_id.clone()),
            importance: parse_importance(&importance)?,
            freshness_at,
            created_at,
            state: parse_entry_state(&entry_state)?,
        },
        current_revision: MemoryRevision {
            revision_id: MemoryRevisionId(current_revision_id),
            memory_id: memory_id.clone(),
            content,
            event_time,
            recorded_at,
            valid_from,
            valid_to,
            change_type: parse_change_type(&change_type)?,
            change_reason,
            source,
            safety_policy_version,
            state: parse_revision_state(&revision_state)?,
        },
    }))
}

fn load_revision_source(
    connection: &Connection,
    scope: &MemoryPersonaScope,
    memory_id: &MemoryId,
    revision_id: &MemoryRevisionId,
) -> Result<MemorySourceEvidence, MemoryError> {
    let source = connection
        .query_row(
            "SELECT source_kind, conversation_id, turn_id, action_id, authorized_at
             FROM memory_revision_source
             WHERE persona_id = ?1
               AND memory_id = ?2
               AND revision_id = ?3
               AND source_ordinal = 0",
            params![scope.persona_id(), memory_id.0, revision_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(repository_unavailable)?;
    match source {
        (kind, Some(conversation_id), Some(turn_id), None, None) => {
            Ok(MemorySourceEvidence::ConversationTurn {
                conversation_id,
                turn_id,
                kind: parse_source_kind(&kind)?,
            })
        }
        (kind, None, None, Some(action_id), Some(authorized_at))
            if kind == "persona_management" =>
        {
            Ok(MemorySourceEvidence::PersonaManagement {
                action_id,
                authorized_at,
            })
        }
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn apply_transition(
    connection: &Connection,
    authority: &SqliteMemoryDeletionAuthority,
    scope: &MemoryPersonaScope,
    transition: &MemoryMutationTransition,
) -> Result<(), MemoryError> {
    if transition.entry.persona_id != scope.persona_id()
        || transition.new_revision.memory_id != transition.entry.memory_id
    {
        return Err(MemoryError::new(MemoryErrorCode::PersonaScopeMismatch));
    }
    if let Some(previous) = &transition.previous_revision {
        let updated = connection
            .execute(
                "UPDATE memory_revision
                 SET valid_to = ?1, state = ?2
                 WHERE persona_id = ?3
                   AND memory_id = ?4
                   AND revision_id = ?5
                   AND state = 'current'",
                params![
                    previous.valid_to,
                    revision_state_to_str(previous.state),
                    scope.persona_id(),
                    previous.memory_id.0,
                    previous.revision_id.0
                ],
            )
            .map_err(repository_unavailable)?;
        if updated != 1 {
            return Err(MemoryError::new(MemoryErrorCode::RevisionConflict));
        }
    } else {
        connection
            .execute(
                "INSERT INTO memory_entry(
                    persona_id, memory_id, category, current_revision_id,
                    importance, freshness_at, created_at, state
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    scope.persona_id(),
                    transition.entry.memory_id.0,
                    category_to_str(transition.entry.category),
                    transition.entry.current_revision_id.0,
                    importance_to_str(transition.entry.importance),
                    transition.entry.freshness_at,
                    transition.entry.created_at,
                    entry_state_to_str(transition.entry.state)
                ],
            )
            .map_err(repository_unavailable)?;
    }

    let row_id = insert_revision(connection, authority, scope, &transition.new_revision)?;
    if let Some(previous) = &transition.previous_revision {
        let updated = connection
            .execute(
                "UPDATE memory_entry
                 SET category = ?1,
                     current_revision_id = ?2,
                     importance = ?3,
                     freshness_at = ?4,
                     state = ?5
                 WHERE persona_id = ?6
                   AND memory_id = ?7
                   AND current_revision_id = ?8",
                params![
                    category_to_str(transition.entry.category),
                    transition.entry.current_revision_id.0,
                    importance_to_str(transition.entry.importance),
                    transition.entry.freshness_at,
                    entry_state_to_str(transition.entry.state),
                    scope.persona_id(),
                    transition.entry.memory_id.0,
                    previous.revision_id.0
                ],
            )
            .map_err(repository_unavailable)?;
        if updated != 1 {
            return Err(MemoryError::new(MemoryErrorCode::RevisionConflict));
        }
    }
    replace_search_projection(connection, scope, transition, row_id)
}

fn insert_revision(
    connection: &Connection,
    authority: &SqliteMemoryDeletionAuthority,
    scope: &MemoryPersonaScope,
    revision: &MemoryRevision,
) -> Result<i64, MemoryError> {
    let canonical = canonicalize_derivation_content(&revision.content);
    let derivation_key = authority.derivation_key(scope.persona_id(), &canonical);
    connection
        .execute(
            "INSERT INTO memory_revision(
                persona_id, memory_id, revision_id, content, derivation_key,
                event_time, recorded_at, valid_from, valid_to, change_type,
                change_reason, safety_policy_version, state
             ) VALUES(
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
             )",
            params![
                scope.persona_id(),
                revision.memory_id.0,
                revision.revision_id.0,
                revision.content,
                derivation_key.as_bytes().as_slice(),
                revision.event_time,
                revision.recorded_at,
                revision.valid_from,
                revision.valid_to,
                change_type_to_str(revision.change_type),
                revision.change_reason,
                revision.safety_policy_version,
                revision_state_to_str(revision.state)
            ],
        )
        .map_err(repository_unavailable)?;
    let row_id = connection.last_insert_rowid();
    insert_revision_source(connection, scope, revision)?;
    Ok(row_id)
}

fn insert_revision_source(
    connection: &Connection,
    scope: &MemoryPersonaScope,
    revision: &MemoryRevision,
) -> Result<(), MemoryError> {
    let (kind, conversation_id, turn_id, action_id, authorized_at) = match &revision.source {
        MemorySourceEvidence::ConversationTurn {
            conversation_id,
            turn_id,
            kind,
        } => (
            source_kind_to_str(*kind),
            Some(conversation_id.as_str()),
            Some(turn_id.as_str()),
            None,
            None,
        ),
        MemorySourceEvidence::PersonaManagement {
            action_id,
            authorized_at,
        } => (
            "persona_management",
            None,
            None,
            Some(action_id.as_str()),
            Some(authorized_at.as_str()),
        ),
    };
    connection
        .execute(
            "INSERT INTO memory_revision_source(
                persona_id, memory_id, revision_id, source_ordinal, source_kind,
                conversation_id, turn_id, action_id, authorized_at
             ) VALUES(?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8)",
            params![
                scope.persona_id(),
                revision.memory_id.0,
                revision.revision_id.0,
                kind,
                conversation_id,
                turn_id,
                action_id,
                authorized_at
            ],
        )
        .map_err(repository_unavailable)?;
    Ok(())
}

fn replace_search_projection(
    connection: &Connection,
    scope: &MemoryPersonaScope,
    transition: &MemoryMutationTransition,
    row_id: i64,
) -> Result<(), MemoryError> {
    connection
        .execute(
            "DELETE FROM memory_search_projection
             WHERE persona_id = ?1 AND memory_id = ?2",
            params![scope.persona_id(), transition.entry.memory_id.0],
        )
        .map_err(repository_unavailable)?;
    connection
        .execute(
            "INSERT INTO memory_search_projection(
                row_id, persona_id, memory_id, revision_id, content, category
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row_id,
                scope.persona_id(),
                transition.entry.memory_id.0,
                transition.new_revision.revision_id.0,
                normalize_search_text(&transition.new_revision.content),
                category_to_str(transition.entry.category)
            ],
        )
        .map_err(repository_unavailable)?;
    Ok(())
}

fn mutation_deletion_check(
    authority: &SqliteMemoryDeletionAuthority,
    scope: &MemoryPersonaScope,
    memory_id: &MemoryId,
    content: &str,
    source: &MemorySourceEvidence,
) -> Result<MemoryDeletionCheckRequest, MemoryError> {
    let mut subjects = BTreeSet::from([
        MemoryDeletionSubject::Persona {
            persona_id: scope.persona_id().to_string(),
        },
        MemoryDeletionSubject::Memory {
            persona_id: scope.persona_id().to_string(),
            memory_id: memory_id.clone(),
        },
        MemoryDeletionSubject::Derivation {
            persona_id: scope.persona_id().to_string(),
            derivation_key: authority.derivation_key(
                scope.persona_id(),
                &canonicalize_derivation_content(content),
            ),
        },
    ]);
    if let MemorySourceEvidence::ConversationTurn {
        conversation_id,
        turn_id,
        ..
    } = source
    {
        subjects.insert(MemoryDeletionSubject::SourceTurn {
            persona_id: scope.persona_id().to_string(),
            conversation_id: conversation_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    MemoryDeletionCheckRequest::new(subjects)
}

fn record_is_blocked(
    guard: &super::authority::CanonicalAuthorityGuard<'_>,
    authority: &SqliteMemoryDeletionAuthority,
    scope: &MemoryPersonaScope,
    record: &MemoryRecord,
) -> Result<bool, MemoryError> {
    let request = read_deletion_check(
        authority,
        scope,
        &record.entry.memory_id,
        &record.current_revision.content,
    )?;
    Ok(matches!(
        guard.check(&request)?,
        MemoryDeletionDecision::Blocked { .. }
    ))
}

fn read_deletion_check(
    authority: &SqliteMemoryDeletionAuthority,
    scope: &MemoryPersonaScope,
    memory_id: &MemoryId,
    content: &str,
) -> Result<MemoryDeletionCheckRequest, MemoryError> {
    MemoryDeletionCheckRequest::new(BTreeSet::from([
        MemoryDeletionSubject::Persona {
            persona_id: scope.persona_id().to_string(),
        },
        MemoryDeletionSubject::Memory {
            persona_id: scope.persona_id().to_string(),
            memory_id: memory_id.clone(),
        },
        MemoryDeletionSubject::Derivation {
            persona_id: scope.persona_id().to_string(),
            derivation_key: authority.derivation_key(
                scope.persona_id(),
                &canonicalize_derivation_content(content),
            ),
        },
    ]))
}

fn require_deletion_allowed(decision: MemoryDeletionDecision) -> Result<(), MemoryError> {
    match decision {
        MemoryDeletionDecision::Allowed => Ok(()),
        MemoryDeletionDecision::Blocked { .. } => {
            Err(MemoryError::new(MemoryErrorCode::SourceIneligible))
        }
    }
}

fn envelope_digest(
    authority: &SqliteMemoryDeletionAuthority,
    envelope: &MemoryCommitEnvelope,
) -> Result<[u8; 32], MemoryError> {
    let mut canonical = Vec::new();
    append_canonical(&mut canonical, envelope.idempotency_key().as_bytes());
    append_canonical(&mut canonical, envelope.scope().persona_id().as_bytes());
    append_canonical(&mut canonical, envelope.conversation_id().as_bytes());
    append_canonical(&mut canonical, envelope.turn_id().as_bytes());
    append_canonical(&mut canonical, envelope.committed_at().as_bytes());
    for mutation in envelope.mutations() {
        append_canonical(&mut canonical, mutation.binding().operation_id().as_bytes());
        append_canonical(&mut canonical, mutation.assigned_memory_id().0.as_bytes());
        append_canonical(&mut canonical, mutation.assigned_revision_id().0.as_bytes());
        append_canonical(&mut canonical, mutation.staging_policy_version().as_bytes());
        append_canonical(
            &mut canonical,
            &serde_json::to_vec(mutation.params())
                .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?,
        );
        append_canonical(
            &mut canonical,
            &serde_json::to_vec(mutation.binding().source())
                .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?,
        );
    }
    Ok(authority.keyed_digest(
        b"muse-memory-committed-envelope/v1",
        &[canonical.as_slice()],
    ))
}

fn management_content_digest(
    authority: &SqliteMemoryDeletionAuthority,
    mutation: &MemoryManagementContentMutation,
) -> Result<[u8; 32], MemoryError> {
    let mut canonical = Vec::new();
    append_canonical(
        &mut canonical,
        mutation.binding().scope().persona_id().as_bytes(),
    );
    append_canonical(&mut canonical, mutation.binding().operation_id().as_bytes());
    append_canonical(&mut canonical, mutation.assigned_memory_id().0.as_bytes());
    append_canonical(&mut canonical, mutation.assigned_revision_id().0.as_bytes());
    append_canonical(&mut canonical, mutation.staging_policy_version().as_bytes());
    append_management_params(&mut canonical, mutation.params());
    append_canonical(
        &mut canonical,
        &serde_json::to_vec(mutation.binding().source())
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?,
    );
    Ok(authority.keyed_digest(
        b"muse-memory-management-content/v1",
        &[canonical.as_slice()],
    ))
}

fn append_management_params(canonical: &mut Vec<u8>, params: &MemoryManagementContentParams) {
    append_canonical(canonical, change_type_to_str(params.operation()).as_bytes());
    append_canonical(canonical, category_to_str(params.category()).as_bytes());
    append_canonical(canonical, params.content().as_bytes());
    append_optional_canonical(canonical, params.importance().map(importance_to_str));
    append_optional_canonical(canonical, params.event_time());
    append_canonical(canonical, params.change_reason().as_bytes());
    append_optional_canonical(
        canonical,
        params.memory_id().map(|memory_id| memory_id.0.as_str()),
    );
    append_optional_canonical(
        canonical,
        params
            .expected_revision_id()
            .map(|revision_id| revision_id.0.as_str()),
    );
}

fn append_optional_canonical(target: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            target.push(1);
            append_canonical(target, value.as_bytes());
        }
        None => target.push(0),
    }
}

fn importance_digest(
    authority: &SqliteMemoryDeletionAuthority,
    adjustment: &MemoryImportanceAdjustment,
) -> [u8; 32] {
    authority.keyed_digest(
        b"muse-memory-management-importance/v1",
        &[
            adjustment.binding().scope().persona_id().as_bytes(),
            adjustment.binding().operation_id().as_bytes(),
            adjustment.memory_id().0.as_bytes(),
            adjustment.expected_revision_id().0.as_bytes(),
            importance_to_str(adjustment.expected_importance()).as_bytes(),
            importance_to_str(adjustment.importance()).as_bytes(),
        ],
    )
}

fn append_canonical(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

fn load_batch_receipt(
    connection: &Connection,
    envelope: &MemoryCommitEnvelope,
    digest: &[u8; 32],
) -> Result<Option<MemoryBatchCommitReceipt>, MemoryError> {
    let existing = connection
        .query_row(
            "SELECT conversation_id, turn_id, envelope_digest, durable_at
             FROM memory_committed_batch
             WHERE persona_id = ?1 AND idempotency_key = ?2",
            params![envelope.scope().persona_id(), envelope.idempotency_key()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(repository_unavailable)?;
    let Some((conversation_id, turn_id, stored_digest, durable_at)) = existing else {
        return Ok(None);
    };
    if conversation_id != envelope.conversation_id()
        || turn_id != envelope.turn_id()
        || stored_digest.as_slice() != digest
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    if chrono::DateTime::parse_from_rfc3339(&durable_at).is_err() {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    let mut statement = connection
        .prepare(
            "SELECT operation_ordinal, operation_id, change_type, memory_id, revision_id
             FROM memory_committed_operation
             WHERE persona_id = ?1 AND idempotency_key = ?2
             ORDER BY operation_ordinal",
        )
        .map_err(repository_unavailable)?;
    let stored_mutations = statement
        .query_map(
            params![envelope.scope().persona_id(), envelope.idempotency_key()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(repository_unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(repository_unavailable)?;
    if stored_mutations.len() != envelope.mutations().len() {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    let mut mutations = Vec::with_capacity(stored_mutations.len());
    for (index, (stored, expected)) in stored_mutations
        .into_iter()
        .zip(envelope.mutations())
        .enumerate()
    {
        let (ordinal, operation_id, operation, memory_id, revision_id) = stored;
        let parsed_operation = parse_change_type(&operation)?;
        if ordinal != index as i64
            || operation_id != expected.binding().operation_id()
            || parsed_operation != expected.params().operation()
            || memory_id != expected.assigned_memory_id().0
            || revision_id != expected.assigned_revision_id().0
        {
            return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
        }
        mutations.push(MemoryMutationReceipt {
            operation: parsed_operation,
            memory_id: MemoryId(memory_id),
            revision_id: MemoryRevisionId(revision_id),
            state: MemoryMutationReceiptState::Durable,
        });
    }
    Ok(Some(MemoryBatchCommitReceipt {
        idempotency_key: envelope.idempotency_key().to_string(),
        mutations,
        durable_at,
    }))
}

fn reject_duplicate_committed_turn(
    connection: &Connection,
    envelope: &MemoryCommitEnvelope,
) -> Result<(), MemoryError> {
    let existing = connection
        .query_row(
            "SELECT idempotency_key
             FROM memory_committed_batch
             WHERE persona_id = ?1 AND conversation_id = ?2 AND turn_id = ?3",
            params![
                envelope.scope().persona_id(),
                envelope.conversation_id(),
                envelope.turn_id()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(repository_unavailable)?;
    if existing.is_some() {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    Ok(())
}

fn load_management_content_receipt(
    connection: &Connection,
    mutation: &MemoryManagementContentMutation,
    digest: &[u8; 32],
) -> Result<Option<MemoryMutationReceipt>, MemoryError> {
    let scope = mutation.binding().scope();
    let operation_id = mutation.binding().operation_id();
    let existing = connection
        .query_row(
            "SELECT operation_kind, mutation_digest, change_type, memory_id, revision_id
             FROM memory_management_operation
             WHERE persona_id = ?1 AND operation_id = ?2",
            params![scope.persona_id(), operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(repository_unavailable)?;
    let Some((kind, stored_digest, change_type, memory_id, revision_id)) = existing else {
        return Ok(None);
    };
    if kind != "content" || stored_digest.as_slice() != digest {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    let operation = parse_change_type(
        change_type
            .as_deref()
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?,
    )?;
    let revision_id =
        revision_id.ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;
    if operation != mutation.params().operation()
        || memory_id.as_str() != mutation.assigned_memory_id().0.as_str()
        || revision_id.as_str() != mutation.assigned_revision_id().0.as_str()
    {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    Ok(Some(MemoryMutationReceipt {
        operation,
        memory_id: MemoryId(memory_id),
        revision_id: MemoryRevisionId(revision_id),
        state: MemoryMutationReceiptState::Durable,
    }))
}

fn load_importance_receipt(
    connection: &Connection,
    adjustment: &MemoryImportanceAdjustment,
    digest: &[u8; 32],
) -> Result<Option<MemoryImportanceAdjustmentReceipt>, MemoryError> {
    let scope = adjustment.binding().scope();
    let operation_id = adjustment.binding().operation_id();
    let existing = connection
        .query_row(
            "SELECT operation_kind, mutation_digest, memory_id,
                    previous_importance, importance, durable_at
             FROM memory_management_operation
             WHERE persona_id = ?1 AND operation_id = ?2",
            params![scope.persona_id(), operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(repository_unavailable)?;
    let Some((kind, stored_digest, memory_id, previous, importance, durable_at)) = existing else {
        return Ok(None);
    };
    if kind != "importance" || stored_digest.as_slice() != digest {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    let previous_importance = parse_importance(
        previous
            .as_deref()
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?,
    )?;
    let importance = parse_importance(
        importance
            .as_deref()
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?,
    )?;
    if memory_id.as_str() != adjustment.memory_id().0.as_str()
        || previous_importance != adjustment.expected_importance()
        || importance != adjustment.importance()
        || chrono::DateTime::parse_from_rfc3339(&durable_at).is_err()
    {
        return Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable));
    }
    Ok(Some(MemoryImportanceAdjustmentReceipt {
        operation_id: operation_id.to_string(),
        memory_id: MemoryId(memory_id),
        previous_importance,
        importance,
        durable_at,
    }))
}

fn category_to_str(value: MemoryCategory) -> &'static str {
    match value {
        MemoryCategory::UserFact => "user_fact",
        MemoryCategory::UserPreference => "user_preference",
        MemoryCategory::SharedExperience => "shared_experience",
        MemoryCategory::Commitment => "commitment",
        MemoryCategory::StoryState => "story_state",
    }
}

fn parse_category(value: &str) -> Result<MemoryCategory, MemoryError> {
    match value {
        "user_fact" => Ok(MemoryCategory::UserFact),
        "user_preference" => Ok(MemoryCategory::UserPreference),
        "shared_experience" => Ok(MemoryCategory::SharedExperience),
        "commitment" => Ok(MemoryCategory::Commitment),
        "story_state" => Ok(MemoryCategory::StoryState),
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn importance_to_str(value: MemoryImportance) -> &'static str {
    match value {
        MemoryImportance::Low => "low",
        MemoryImportance::Normal => "normal",
        MemoryImportance::High => "high",
    }
}

fn parse_importance(value: &str) -> Result<MemoryImportance, MemoryError> {
    match value {
        "low" => Ok(MemoryImportance::Low),
        "normal" => Ok(MemoryImportance::Normal),
        "high" => Ok(MemoryImportance::High),
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn entry_state_to_str(value: MemoryEntryState) -> &'static str {
    match value {
        MemoryEntryState::Active => "active",
        MemoryEntryState::Deleted => "deleted",
    }
}

fn parse_entry_state(value: &str) -> Result<MemoryEntryState, MemoryError> {
    match value {
        "active" => Ok(MemoryEntryState::Active),
        "deleted" => Ok(MemoryEntryState::Deleted),
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn change_type_to_str(value: MemoryChangeType) -> &'static str {
    match value {
        MemoryChangeType::Create => "create",
        MemoryChangeType::Update => "update",
        MemoryChangeType::Correct => "correct",
    }
}

fn parse_change_type(value: &str) -> Result<MemoryChangeType, MemoryError> {
    match value {
        "create" => Ok(MemoryChangeType::Create),
        "update" => Ok(MemoryChangeType::Update),
        "correct" => Ok(MemoryChangeType::Correct),
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn revision_state_to_str(value: MemoryRevisionState) -> &'static str {
    match value {
        MemoryRevisionState::Current => "current",
        MemoryRevisionState::Superseded => "superseded",
        MemoryRevisionState::Corrected => "corrected",
    }
}

fn parse_revision_state(value: &str) -> Result<MemoryRevisionState, MemoryError> {
    match value {
        "current" => Ok(MemoryRevisionState::Current),
        "superseded" => Ok(MemoryRevisionState::Superseded),
        "corrected" => Ok(MemoryRevisionState::Corrected),
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn source_kind_to_str(value: MemorySourceKind) -> &'static str {
    match value {
        MemorySourceKind::DirectUserMessage => "direct_user_message",
        MemorySourceKind::UserConfirmation => "user_confirmation",
        MemorySourceKind::DeterministicLocalEvent => "deterministic_local_event",
    }
}

fn parse_source_kind(value: &str) -> Result<MemorySourceKind, MemoryError> {
    match value {
        "direct_user_message" => Ok(MemorySourceKind::DirectUserMessage),
        "user_confirmation" => Ok(MemorySourceKind::UserConfirmation),
        "deterministic_local_event" => Ok(MemorySourceKind::DeterministicLocalEvent),
        _ => Err(MemoryError::new(MemoryErrorCode::RepositoryUnavailable)),
    }
}

fn repository_unavailable<T>(_error: T) -> MemoryError {
    MemoryError::new(MemoryErrorCode::RepositoryUnavailable)
}
