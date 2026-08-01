//! Persona 长期记忆的独立删除权威。
//!
//! 普通记忆正文属于 `runtime/muse.sqlite` 的备份域；前向删除权威与派生密钥
//! 位于独立 `privacy/` 恢复域，恢复旧主库时不得被普通备份覆盖。

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::app::storage::{
    atomic_write_sensitive_synced, create_unique_temporary_path, prepare_runtime_database_path,
    replace_file, restrict_sensitive_file_permissions, sync_parent_directory_required,
};
use crate::domain::memory::{
    MAX_MEMORY_DELETION_FIELD_BYTES, MAX_MEMORY_DELETION_SUBJECTS, MemoryDeletionAuthority,
    MemoryDeletionAuthorityReceipt, MemoryDeletionAuthorityRequest, MemoryDeletionCheckRequest,
    MemoryDeletionDecision, MemoryDeletionSubject, MemoryDerivationKey, MemoryError,
    MemoryErrorCode,
};

const MEMORY_PRIVACY_DIRECTORY: &str = "privacy";
const MEMORY_DELETION_AUTHORITY_FILE: &str = "memory-deletion-authority.sqlite";
const MEMORY_DERIVATION_KEY_FILE: &str = "memory-derivation.key";
const MEMORY_DERIVATION_KEY_LENGTH: usize = 32;
const MEMORY_AUTHORITY_SCHEMA_VERSION: &str = "muse-memory-deletion-authority/v1";
const MEMORY_AUTHORITY_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_AUTHORITY_ROWS: usize = MAX_MEMORY_DELETION_SUBJECTS * 8;
const MAX_AUTHORITY_MATERIALIZED_BYTES: usize = 2 * 1024 * 1024;
const MAX_AUTHORITY_ROW_BYTES: usize = MAX_MEMORY_DELETION_FIELD_BYTES * 8 + 64;
const AUTHORITY_READ_BATCH_SIZE: usize = 64;
const AUTHORITY_SQLITE_PROGRESS_INTERVAL_OPS: i32 = 1_000;
const MAX_AUTHORITY_SQLITE_PROGRESS_CALLBACKS: usize = 20_000;
#[cfg(test)]
static FAIL_NEXT_AUTHORITY_SYNCS: Mutex<Option<(PathBuf, usize)>> = Mutex::new(None);
#[cfg(test)]
static PAUSE_AFTER_AUTHORITY_COMMIT: Mutex<Option<AuthorityCommitWindowHook>> = Mutex::new(None);

#[cfg(test)]
struct AuthorityCommitWindowHook {
    database_path: PathBuf,
    entered: SyncSender<()>,
    resume: Receiver<()>,
}

const MEMORY_AUTHORITY_SCHEMA: &str = r#"
    CREATE TABLE authority_meta (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        authority_id TEXT NOT NULL,
        schema_version TEXT NOT NULL,
        created_at TEXT NOT NULL,
        key_verifier BLOB NOT NULL CHECK (length(key_verifier) = 32),
        ledger_commitment BLOB NOT NULL CHECK (length(ledger_commitment) = 32)
    );
    CREATE TABLE deletion_event (
        authority_revision INTEGER PRIMARY KEY AUTOINCREMENT,
        deletion_id TEXT NOT NULL,
        persona_id TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        durable_at TEXT NOT NULL,
        request_kind TEXT CHECK (
            request_kind IS NULL OR request_kind IN ('memory', 'persona_all')
        ),
        requested_memory_id TEXT,
        target_memory_count INTEGER CHECK (
            target_memory_count IS NULL OR target_memory_count >= 0
        ),
        cleanup_completed_at TEXT,
        deleted_memory_count INTEGER CHECK (
            deleted_memory_count IS NULL OR deleted_memory_count >= 0
        ),
        event_verifier BLOB NOT NULL CHECK (length(event_verifier) = 32),
        CHECK (
            (
                request_kind IS NULL
                AND requested_memory_id IS NULL
                AND target_memory_count IS NULL
            )
            OR (
                request_kind = 'memory'
                AND requested_memory_id IS NOT NULL
                AND target_memory_count IS NOT NULL
            )
            OR (
                request_kind = 'persona_all'
                AND requested_memory_id IS NULL
                AND target_memory_count IS NOT NULL
            )
        ),
        CHECK (
            cleanup_completed_at IS NULL
            OR (
                request_kind IS NOT NULL
                AND
                target_memory_count IS NOT NULL
                AND deleted_memory_count = target_memory_count
            )
        ),
        UNIQUE(persona_id, deletion_id)
    );
    CREATE INDEX deletion_event_persona_revision
        ON deletion_event(persona_id, authority_revision);
    CREATE TABLE deletion_subject (
        deletion_id TEXT NOT NULL,
        persona_id TEXT NOT NULL,
        subject_fingerprint BLOB NOT NULL CHECK (length(subject_fingerprint) = 32),
        subject_kind TEXT NOT NULL CHECK (
            subject_kind IN ('persona', 'memory', 'source_turn', 'derivation')
        ),
        memory_id TEXT,
        conversation_id TEXT,
        turn_id TEXT,
        derivation_key BLOB CHECK (
            derivation_key IS NULL OR length(derivation_key) = 32
        ),
        PRIMARY KEY(persona_id, deletion_id, subject_fingerprint),
        FOREIGN KEY(persona_id, deletion_id)
            REFERENCES deletion_event(persona_id, deletion_id)
            ON DELETE CASCADE,
        CHECK (
            (
                subject_kind = 'persona'
                AND memory_id IS NULL
                AND conversation_id IS NULL
                AND turn_id IS NULL
                AND derivation_key IS NULL
            )
            OR (
                subject_kind = 'memory'
                AND memory_id IS NOT NULL
                AND conversation_id IS NULL
                AND turn_id IS NULL
                AND derivation_key IS NULL
            )
            OR (
                subject_kind = 'source_turn'
                AND memory_id IS NULL
                AND conversation_id IS NOT NULL
                AND turn_id IS NOT NULL
                AND derivation_key IS NULL
            )
            OR (
                subject_kind = 'derivation'
                AND memory_id IS NULL
                AND conversation_id IS NULL
                AND turn_id IS NULL
                AND derivation_key IS NOT NULL
            )
        )
    );
    CREATE INDEX deletion_subject_persona_kind
        ON deletion_subject(persona_id, subject_kind);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityAnchor {
    pub authority_id: String,
    pub initialized_at: String,
    pub last_applied_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityDeletionEvent {
    pub authority_revision: i64,
    pub deletion_id: String,
    pub persona_id: String,
    pub recorded_at: String,
    pub subjects: BTreeSet<MemoryDeletionSubject>,
    pub request_kind: Option<String>,
    pub requested_memory_id: Option<String>,
    pub target_memory_count: Option<u64>,
    pub cleanup_completed_at: Option<String>,
}

impl AuthorityDeletionEvent {
    fn validate_capacity(&self) -> Result<(), MemoryError> {
        if self.subjects.is_empty() || self.subjects.len() > MAX_MEMORY_DELETION_SUBJECTS {
            return Err(authority_limit_exceeded());
        }
        for value in [
            Some(self.deletion_id.as_str()),
            Some(self.persona_id.as_str()),
            Some(self.recorded_at.as_str()),
            self.request_kind.as_deref(),
            self.requested_memory_id.as_deref(),
            self.cleanup_completed_at.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.is_empty() || value.len() > MAX_MEMORY_DELETION_FIELD_BYTES {
                return Err(authority_limit_exceeded());
            }
        }
        for subject in &self.subjects {
            subject.validate().map_err(|_| authority_limit_exceeded())?;
        }
        Ok(())
    }
}

pub(crate) struct AuthorityDeleteIntent<'a> {
    pub request_kind: &'a str,
    pub requested_memory_id: Option<&'a str>,
    pub target_memory_count: u64,
}

#[derive(Clone)]
struct StoredAuthorityEvent {
    authority_revision: i64,
    deletion_id: String,
    persona_id: String,
    recorded_at: String,
    durable_at: String,
    request_kind: Option<String>,
    requested_memory_id: Option<String>,
    target_memory_count: Option<i64>,
    cleanup_completed_at: Option<String>,
    deleted_memory_count: Option<i64>,
    event_verifier: Vec<u8>,
}

/// 位于普通 runtime SQLite 备份域之外的前向删除权威。
pub struct SqliteMemoryDeletionAuthority {
    database_path: PathBuf,
    derivation_key: [u8; MEMORY_DERIVATION_KEY_LENGTH],
}

pub(crate) struct CanonicalAuthorityGuard<'a> {
    authority: &'a SqliteMemoryDeletionAuthority,
    connection: Connection,
    budget: AuthorityReadBudget,
    exhausted: Arc<AtomicBool>,
    finished: bool,
}

struct AuthorityReadBudget {
    rows: usize,
    materialized_bytes: usize,
}

struct AuthorityTransaction<'a> {
    authority: &'a SqliteMemoryDeletionAuthority,
    connection: &'a Connection,
    budget: &'a mut AuthorityReadBudget,
}

impl AuthorityReadBudget {
    const fn new() -> Self {
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
            total
                .checked_add(*length)
                .ok_or_else(authority_limit_exceeded)
        })?;
        if row_bytes > maximum_row_bytes {
            return Err(authority_limit_exceeded());
        }
        let rows = self
            .rows
            .checked_add(1)
            .ok_or_else(authority_limit_exceeded)?;
        let materialized_bytes = self
            .materialized_bytes
            .checked_add(row_bytes)
            .ok_or_else(authority_limit_exceeded)?;
        if rows > MAX_AUTHORITY_ROWS || materialized_bytes > MAX_AUTHORITY_MATERIALIZED_BYTES {
            return Err(authority_limit_exceeded());
        }
        self.rows = rows;
        self.materialized_bytes = materialized_bytes;
        Ok(())
    }
}

impl CanonicalAuthorityGuard<'_> {
    pub(crate) fn check(
        &mut self,
        request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        check_with_connection(self.authority, &self.connection, request)
    }

    pub(crate) fn event(
        &mut self,
        persona_id: &str,
        deletion_id: &str,
    ) -> Result<Option<AuthorityDeletionEvent>, MemoryError> {
        load_event_with_connection(
            &self.authority.derivation_key,
            &self.connection,
            persona_id,
            deletion_id,
            &mut self.budget,
        )
    }

    /// confirmation ID 在整个删除权威中全局唯一；跨 Persona 复用必须稳定拒绝，
    /// 不能因旧 schema 的复合唯一键而被当成另一条独立确认。
    pub(crate) fn event_by_deletion_id(
        &mut self,
        deletion_id: &str,
    ) -> Result<Option<AuthorityDeletionEvent>, MemoryError> {
        let Some(persona_id) = event_persona_by_deletion_id(
            &self.connection,
            deletion_id,
            &mut self.budget,
        )? else {
            return Ok(None);
        };
        load_event_with_connection(
            &self.authority.derivation_key,
            &self.connection,
            &persona_id,
            deletion_id,
            &mut self.budget,
        )
    }

    pub(crate) fn pending_events(
        &mut self,
        last_applied_revision: i64,
    ) -> Result<Vec<AuthorityDeletionEvent>, MemoryError> {
        load_pending_events(
            &self.authority.derivation_key,
            &self.connection,
            last_applied_revision,
            &mut self.budget,
        )
    }

    pub(crate) fn max_revision(&mut self) -> Result<i64, MemoryError> {
        self.connection
            .query_row(
                "SELECT COALESCE(MAX(authority_revision), 0) FROM deletion_event",
                [],
                |row| row.get(0),
            )
            .map_err(authority_unavailable)
    }

    pub(crate) fn record_delete_intent_durable(
        &mut self,
        persona_id: &str,
        deletion_id: &str,
        recorded_at: &str,
        subjects: &BTreeSet<MemoryDeletionSubject>,
        intent: AuthorityDeleteIntent<'_>,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        if !matches!(
            (intent.request_kind, intent.requested_memory_id),
            ("memory", Some(_)) | ("persona_all", None)
        ) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        let receipt = upsert_event_in_transaction(
            AuthorityTransaction {
                authority: self.authority,
                connection: &self.connection,
                budget: &mut self.budget,
            },
            persona_id,
            deletion_id,
            recorded_at,
            subjects,
            Some((
                intent.request_kind,
                intent.requested_memory_id,
                intent.target_memory_count,
            )),
        )?;
        self.commit_sync_and_relock()?;
        Ok(receipt)
    }

    fn record_subjects_durable(
        &mut self,
        persona_id: &str,
        deletion_id: &str,
        recorded_at: &str,
        subjects: &BTreeSet<MemoryDeletionSubject>,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        let receipt = upsert_event_in_transaction(
            AuthorityTransaction {
                authority: self.authority,
                connection: &self.connection,
                budget: &mut self.budget,
            },
            persona_id,
            deletion_id,
            recorded_at,
            subjects,
            None,
        )?;
        self.commit_sync_and_relock()?;
        Ok(receipt)
    }

    pub(crate) fn mark_cleanup_completed(
        &mut self,
        persona_id: &str,
        deletion_id: &str,
        deleted_memory_count: u64,
        completed_at: &str,
    ) -> Result<(), MemoryError> {
        let mut existing =
            load_stored_event(&self.connection, persona_id, deletion_id, &mut self.budget)?
                .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable))?;
        validate_stored_event(&self.authority.derivation_key, &existing)?;
        let deleted = i64::try_from(deleted_memory_count)
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?;
        match (
            existing.target_memory_count,
            existing.cleanup_completed_at.as_deref(),
            existing.deleted_memory_count,
        ) {
            (Some(target), Some(_), Some(stored)) if target == deleted && stored == deleted => {
                Ok(())
            }
            (Some(target), None, None) if target == deleted => {
                let previous_verifier = existing.event_verifier.clone();
                existing.cleanup_completed_at = Some(completed_at.to_string());
                existing.deleted_memory_count = Some(deleted);
                let event_verifier =
                    authority_event_verifier(&self.authority.derivation_key, &existing);
                let updated = self
                    .connection
                    .execute(
                        "UPDATE deletion_event
                         SET cleanup_completed_at = ?1,
                             deleted_memory_count = ?2,
                             event_verifier = ?3
                         WHERE persona_id = ?4
                           AND deletion_id = ?5
                           AND cleanup_completed_at IS NULL
                           AND event_verifier = ?6",
                        params![
                            completed_at,
                            deleted,
                            event_verifier.as_slice(),
                            persona_id,
                            deletion_id,
                            previous_verifier
                        ],
                    )
                    .map_err(authority_unavailable)?;
                if updated == 1 {
                    Ok(())
                } else {
                    Err(MemoryError::new(
                        MemoryErrorCode::DeletionAuthorityUnavailable,
                    ))
                }
            }
            _ => Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            )),
        }
    }

    fn commit_sync_and_relock(&mut self) -> Result<(), MemoryError> {
        self.connection
            .execute_batch("COMMIT")
            .map_err(authority_unavailable)?;
        sync_authority_file(&self.authority.database_path)?;
        observe_authority_commit_window(&self.authority.database_path)?;
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(authority_unavailable)
    }

    pub(crate) fn finish(mut self) -> Result<(), MemoryError> {
        self.ensure_not_exhausted()?;
        self.connection
            .execute_batch("COMMIT")
            .map_err(authority_unavailable)?;
        self.finished = true;
        clear_authority_progress_handler(&self.connection);
        Ok(())
    }

    pub(crate) fn finish_durable(mut self) -> Result<(), MemoryError> {
        self.ensure_not_exhausted()?;
        self.connection
            .execute_batch("COMMIT")
            .map_err(authority_unavailable)?;
        sync_authority_file(&self.authority.database_path)?;
        self.finished = true;
        clear_authority_progress_handler(&self.connection);
        Ok(())
    }

    fn ensure_not_exhausted(&self) -> Result<(), MemoryError> {
        if self.exhausted.load(Ordering::Relaxed) {
            Err(authority_limit_exceeded())
        } else {
            Ok(())
        }
    }
}

impl Drop for CanonicalAuthorityGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
        clear_authority_progress_handler(&self.connection);
    }
}

impl fmt::Debug for SqliteMemoryDeletionAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SqliteMemoryDeletionAuthority([独立受保护恢复域])")
    }
}

impl SqliteMemoryDeletionAuthority {
    /// 只允许打开已经由 runtime anchor 绑定的删除权威。
    pub fn open(base_dir: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let runtime_database = base_dir.as_ref().join("runtime").join("muse.sqlite");
        let connection = Connection::open_with_flags(
            runtime_database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(authority_unavailable)?;
        let exhausted = install_authority_progress_handler(&connection);
        let mut budget = AuthorityReadBudget::new();
        let anchor = load_runtime_authority_anchor(&connection, &mut budget);
        clear_authority_progress_handler(&connection);
        if exhausted.load(Ordering::Relaxed) {
            return Err(authority_limit_exceeded());
        }
        let anchor = anchor?;
        drop(connection);
        open_authority_for_anchor(base_dir.as_ref(), &anchor)
    }

    fn open_internal(base_dir: &Path, allow_initialization: bool) -> Result<Self, MemoryError> {
        let privacy_dir = base_dir.join(MEMORY_PRIVACY_DIRECTORY);
        let database_path = privacy_dir.join(MEMORY_DELETION_AUTHORITY_FILE);
        let derivation_key_path = privacy_dir.join(MEMORY_DERIVATION_KEY_FILE);

        prepare_runtime_database_path(&database_path).map_err(authority_unavailable)?;
        let database_length = regular_file_length(&database_path)?;
        let database_has_content = database_length.is_some_and(|length| length > 0);
        let key_existed = regular_file_exists(&derivation_key_path)?;
        if !allow_initialization && (!database_has_content || !key_existed) {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }

        if database_has_content {
            if !key_existed {
                return Err(MemoryError::new(
                    MemoryErrorCode::DeletionAuthorityUnavailable,
                ));
            }
            restrict_sensitive_file_permissions(&derivation_key_path)
                .map_err(authority_unavailable)?;
            let derivation_key = read_derivation_key(&derivation_key_path)?;
            restrict_sensitive_file_permissions(&database_path).map_err(authority_unavailable)?;
            let (connection, _, exhausted) =
                open_validated_authority_connection(&database_path, &derivation_key)?;
            clear_authority_progress_handler(&connection);
            if exhausted.load(Ordering::Relaxed) {
                return Err(authority_limit_exceeded());
            }
            drop(connection);
            sync_authority_file(&database_path)?;
            return Ok(Self {
                database_path,
                derivation_key,
            });
        }

        if !allow_initialization {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }
        let derivation_key = if key_existed {
            restrict_sensitive_file_permissions(&derivation_key_path)
                .map_err(authority_unavailable)?;
            match read_derivation_key(&derivation_key_path) {
                Ok(key) => key,
                Err(_) => publish_new_derivation_key(&derivation_key_path, &privacy_dir)?,
            }
        } else {
            publish_new_derivation_key(&derivation_key_path, &privacy_dir)?
        };
        initialize_authority_database(&database_path, &privacy_dir, &derivation_key)?;
        restrict_sensitive_file_permissions(&database_path).map_err(authority_unavailable)?;
        let (connection, _, exhausted) =
            open_validated_authority_connection(&database_path, &derivation_key)?;
        clear_authority_progress_handler(&connection);
        if exhausted.load(Ordering::Relaxed) {
            return Err(authority_limit_exceeded());
        }
        drop(connection);
        sync_authority_file(&database_path)?;

        Ok(Self {
            database_path,
            derivation_key,
        })
    }

    pub(crate) fn anchor(&self) -> Result<AuthorityAnchor, MemoryError> {
        let (connection, mut budget, exhausted) = self.open_validated_connection()?;
        let result = load_authority_anchor(&connection, &mut budget);
        clear_authority_progress_handler(&connection);
        if exhausted.load(Ordering::Relaxed) {
            return Err(authority_limit_exceeded());
        }
        result
    }

    pub(crate) fn keyed_digest(&self, domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
        keyed_digest_with_key(&self.derivation_key, domain, fields)
    }

    /// 返回不泄露原始正文的受保护派生摘要。输入必须先经
    /// `canonicalize_derivation_content` 处理，不得使用 FTS 的 alphanumeric 归一化。
    pub(crate) fn derivation_key(
        &self,
        persona_id: &str,
        canonical_content: &str,
    ) -> MemoryDerivationKey {
        MemoryDerivationKey::from_digest(self.keyed_digest(
            b"muse-memory-derivation/v1",
            &[persona_id.as_bytes(), canonical_content.as_bytes()],
        ))
    }

    fn subject_fingerprint(&self, subject: &MemoryDeletionSubject) -> [u8; 32] {
        subject_fingerprint_with_key(&self.derivation_key, subject)
    }

    fn open_validated_connection(
        &self,
    ) -> Result<(Connection, AuthorityReadBudget, Arc<AtomicBool>), MemoryError> {
        open_validated_authority_connection(&self.database_path, &self.derivation_key)
    }

    pub(crate) fn begin_guard(&self) -> Result<CanonicalAuthorityGuard<'_>, MemoryError> {
        let (connection, budget, exhausted) = self.open_validated_connection()?;
        if let Err(error) = connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(authority_unavailable)
        {
            clear_authority_progress_handler(&connection);
            return Err(error);
        }
        Ok(CanonicalAuthorityGuard {
            authority: self,
            connection,
            budget,
            exhausted,
            finished: false,
        })
    }
}

fn upsert_event_in_transaction(
    transaction: AuthorityTransaction<'_>,
    persona_id: &str,
    deletion_id: &str,
    recorded_at: &str,
    subjects: &BTreeSet<MemoryDeletionSubject>,
    intent: Option<(&str, Option<&str>, u64)>,
) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
    let AuthorityTransaction {
        authority,
        connection,
        budget,
    } = transaction;
    if subjects.is_empty() || subjects.len() > MAX_MEMORY_DELETION_SUBJECTS {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    for subject in subjects {
        subject.validate()?;
        if subject_persona_id(subject) != persona_id {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
    }
    let requested_intent = intent
        .map(|(kind, memory_id, count)| {
            i64::try_from(count)
                .map(|count| (kind, memory_id, count))
                .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))
        })
        .transpose()?;
    if event_persona_by_deletion_id(connection, deletion_id, budget)?
        .is_some_and(|stored_persona| stored_persona != persona_id)
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    if let Some(mut existing) = load_stored_event(connection, persona_id, deletion_id, budget)? {
        validate_stored_event(&authority.derivation_key, &existing)?;
        let stored_subjects = load_subjects(
            &authority.derivation_key,
            connection,
            persona_id,
            deletion_id,
            budget,
        )?;
        if existing.recorded_at != recorded_at || &stored_subjects != subjects {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        if let Some((request_kind, requested_memory_id, target_count)) = requested_intent {
            match (
                existing.request_kind.as_deref(),
                existing.requested_memory_id.as_deref(),
                existing.target_memory_count,
            ) {
                (Some(kind), memory_id, Some(count))
                    if kind == request_kind
                        && memory_id == requested_memory_id
                        && count == target_count => {}
                (None, None, None) => {
                    let previous_verifier = existing.event_verifier.clone();
                    existing.request_kind = Some(request_kind.to_string());
                    existing.requested_memory_id = requested_memory_id.map(str::to_string);
                    existing.target_memory_count = Some(target_count);
                    let event_verifier =
                        authority_event_verifier(&authority.derivation_key, &existing);
                    let updated = connection
                        .execute(
                            "UPDATE deletion_event
                             SET request_kind = ?1,
                                 requested_memory_id = ?2,
                                 target_memory_count = ?3,
                                 event_verifier = ?4
                             WHERE persona_id = ?5
                               AND deletion_id = ?6
                               AND request_kind IS NULL
                               AND requested_memory_id IS NULL
                               AND target_memory_count IS NULL
                               AND event_verifier = ?7",
                            params![
                                request_kind,
                                requested_memory_id,
                                target_count,
                                event_verifier.as_slice(),
                                persona_id,
                                deletion_id,
                                previous_verifier
                            ],
                        )
                        .map_err(authority_unavailable)?;
                    if updated != 1 {
                        return Err(MemoryError::new(
                            MemoryErrorCode::DeletionAuthorityUnavailable,
                        ));
                    }
                }
                _ => return Err(MemoryError::new(MemoryErrorCode::InvalidRequest)),
            }
        }
        return Ok(MemoryDeletionAuthorityReceipt {
            deletion_id: deletion_id.to_string(),
            authority_revision: authority_revision(existing.authority_revision),
            subjects: subjects.clone(),
            durable_at: existing.durable_at,
        });
    }

    let durable_at = Utc::now().to_rfc3339();
    let (request_kind, requested_memory_id, target_count) = match requested_intent {
        Some((kind, memory_id, count)) => (Some(kind), memory_id, Some(count)),
        None => (None, None, None),
    };
    let placeholder_verifier = [0_u8; 32];
    connection
        .execute(
            "INSERT INTO deletion_event(
                deletion_id, persona_id, recorded_at, durable_at,
                request_kind, requested_memory_id, target_memory_count,
                event_verifier
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                deletion_id,
                persona_id,
                recorded_at,
                durable_at,
                request_kind,
                requested_memory_id,
                target_count,
                placeholder_verifier.as_slice()
            ],
        )
        .map_err(authority_unavailable)?;
    let revision = connection.last_insert_rowid();
    let stored_event = StoredAuthorityEvent {
        authority_revision: revision,
        deletion_id: deletion_id.to_string(),
        persona_id: persona_id.to_string(),
        recorded_at: recorded_at.to_string(),
        durable_at: durable_at.clone(),
        request_kind: request_kind.map(str::to_string),
        requested_memory_id: requested_memory_id.map(str::to_string),
        target_memory_count: target_count,
        cleanup_completed_at: None,
        deleted_memory_count: None,
        event_verifier: placeholder_verifier.to_vec(),
    };
    let event_verifier = authority_event_verifier(&authority.derivation_key, &stored_event);
    let verified = connection
        .execute(
            "UPDATE deletion_event
             SET event_verifier = ?1
             WHERE authority_revision = ?2 AND event_verifier = ?3",
            params![
                event_verifier.as_slice(),
                revision,
                placeholder_verifier.as_slice()
            ],
        )
        .map_err(authority_unavailable)?;
    if verified != 1 {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    for subject in subjects {
        insert_subject(
            connection,
            deletion_id,
            subject,
            authority.subject_fingerprint(subject),
        )?;
    }
    refresh_ledger_commitment(&authority.derivation_key, connection, budget)?;
    Ok(MemoryDeletionAuthorityReceipt {
        deletion_id: deletion_id.to_string(),
        authority_revision: authority_revision(revision),
        subjects: subjects.clone(),
        durable_at,
    })
}

pub(crate) fn prepare_authority_for_migration(
    base_dir: &Path,
) -> Result<AuthorityAnchor, MemoryError> {
    SqliteMemoryDeletionAuthority::open_internal(base_dir, true)?.anchor()
}

pub(crate) fn open_authority_for_anchor(
    base_dir: &Path,
    expected: &AuthorityAnchor,
) -> Result<SqliteMemoryDeletionAuthority, MemoryError> {
    let authority = SqliteMemoryDeletionAuthority::open_internal(base_dir, false)?;
    let actual = authority.anchor()?;
    if actual.authority_id != expected.authority_id
        || actual.initialized_at != expected.initialized_at
        || actual.last_applied_revision < expected.last_applied_revision
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    Ok(authority)
}

impl MemoryDeletionAuthority for SqliteMemoryDeletionAuthority {
    fn record(
        &self,
        request: &MemoryDeletionAuthorityRequest,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        let persona_id = single_subject_persona(request.subjects())?;
        let mut guard = self.begin_guard()?;
        let receipt = guard.record_subjects_durable(
            persona_id,
            request.deletion_id(),
            request.recorded_at(),
            request.subjects(),
        )?;
        guard.finish()?;
        Ok(receipt)
    }

    fn check(
        &self,
        request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        let mut guard = self.begin_guard()?;
        let decision = guard.check(request)?;
        guard.finish()?;
        Ok(decision)
    }
}

fn check_with_connection(
    authority: &SqliteMemoryDeletionAuthority,
    connection: &Connection,
    request: &MemoryDeletionCheckRequest,
) -> Result<MemoryDeletionDecision, MemoryError> {
    let mut matched = BTreeSet::new();
    for subject in request.subjects() {
        let persona_id = subject_persona_id(subject);
        let fingerprint = authority.subject_fingerprint(subject);
        let blocked: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                    FROM deletion_subject
                    WHERE persona_id = ?1 AND subject_fingerprint = ?2
                 )",
                params![persona_id, fingerprint.as_slice()],
                |row| row.get(0),
            )
            .map_err(authority_unavailable)?;
        if blocked {
            matched.insert(subject.clone());
        }
    }
    if matched.is_empty() {
        Ok(MemoryDeletionDecision::Allowed)
    } else {
        Ok(MemoryDeletionDecision::Blocked { matched })
    }
}

fn load_pending_events(
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
    last_applied_revision: i64,
    budget: &mut AuthorityReadBudget,
) -> Result<Vec<AuthorityDeletionEvent>, MemoryError> {
    if last_applied_revision < 0 {
        return Err(authority_unavailable_marker());
    }
    let mut after_revision = 0_i64;
    let mut events = Vec::new();
    loop {
        let mut statement = connection
            .prepare(
                "SELECT authority_revision,
                        CASE WHEN typeof(persona_id) = 'text'
                                  AND length(CAST(persona_id AS BLOB)) <= ?3
                             THEN persona_id END,
                        typeof(persona_id) = 'text', length(CAST(persona_id AS BLOB)),
                        CASE WHEN typeof(deletion_id) = 'text'
                                  AND length(CAST(deletion_id AS BLOB)) <= ?3
                             THEN deletion_id END,
                        typeof(deletion_id) = 'text', length(CAST(deletion_id AS BLOB))
                 FROM deletion_event
                 WHERE authority_revision > ?1
                   AND (
                       authority_revision > ?2
                       OR (request_kind IS NOT NULL AND cleanup_completed_at IS NULL)
                   )
                 ORDER BY authority_revision
                 LIMIT ?4",
            )
            .map_err(authority_unavailable)?;
        let rows = statement
            .query_map(
                params![
                    after_revision,
                    last_applied_revision,
                    MAX_MEMORY_DELETION_FIELD_BYTES as i64,
                    AUTHORITY_READ_BATCH_SIZE as i64
                ],
                |row| {
                    let revision = row.get::<_, i64>(0)?;
                    let persona = required_authority_text(
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let deletion = required_authority_text(
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    Ok((revision, persona, deletion))
                },
            )
            .map_err(authority_unavailable)?;
        let mut identities = Vec::with_capacity(AUTHORITY_READ_BATCH_SIZE);
        for row in rows {
            let (revision, persona, deletion) = row.map_err(authority_unavailable)?;
            if revision <= after_revision {
                return Err(authority_unavailable_marker());
            }
            budget.consume_row(
                &[persona.1, deletion.1],
                MAX_MEMORY_DELETION_FIELD_BYTES * 2,
            )?;
            identities.push((revision, persona.0, deletion.0));
        }
        drop(statement);
        if identities.is_empty() {
            break;
        }
        for (revision, persona_id, deletion_id) in identities {
            let event = load_event_with_connection(
                derivation_key,
                connection,
                &persona_id,
                &deletion_id,
                budget,
            )?
            .ok_or_else(authority_unavailable_marker)?;
            if event.authority_revision != revision {
                return Err(authority_unavailable_marker());
            }
            after_revision = revision;
            events.push(event);
        }
    }
    Ok(events)
}

fn load_event_with_connection(
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
    persona_id: &str,
    deletion_id: &str,
    budget: &mut AuthorityReadBudget,
) -> Result<Option<AuthorityDeletionEvent>, MemoryError> {
    let Some(stored) = load_stored_event(connection, persona_id, deletion_id, budget)? else {
        return Ok(None);
    };
    validate_stored_event(derivation_key, &stored)?;
    let target_memory_count = stored
        .target_memory_count
        .map(|value| u64::try_from(value).map_err(authority_unavailable))
        .transpose()?;
    let deleted_memory_count = stored
        .deleted_memory_count
        .map(|value| u64::try_from(value).map_err(authority_unavailable))
        .transpose()?;
    if stored.cleanup_completed_at.is_some()
        && (target_memory_count.is_none() || deleted_memory_count != target_memory_count)
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    let subjects = load_subjects(derivation_key, connection, persona_id, deletion_id, budget)?;
    let event = AuthorityDeletionEvent {
        authority_revision: stored.authority_revision,
        deletion_id: stored.deletion_id,
        persona_id: stored.persona_id,
        recorded_at: stored.recorded_at,
        subjects,
        request_kind: stored.request_kind,
        requested_memory_id: stored.requested_memory_id,
        target_memory_count,
        cleanup_completed_at: stored.cleanup_completed_at,
    };
    event.validate_capacity()?;
    Ok(Some(event))
}

fn event_persona_by_deletion_id(
    connection: &Connection,
    deletion_id: &str,
    budget: &mut AuthorityReadBudget,
) -> Result<Option<String>, MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN typeof(persona_id) = 'text'
                              AND length(CAST(persona_id AS BLOB)) <= ?2
                         THEN persona_id END,
                    typeof(persona_id) = 'text',
                    length(CAST(persona_id AS BLOB))
             FROM deletion_event
             WHERE deletion_id = ?1
             ORDER BY authority_revision
             LIMIT 2",
        )
        .map_err(authority_unavailable)?;
    let rows = statement
        .query_map(
            params![deletion_id, MAX_MEMORY_DELETION_FIELD_BYTES as i64],
            |row| {
                required_authority_text(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )
            },
        )
        .map_err(authority_unavailable)?;
    let mut personas = Vec::with_capacity(2);
    for row in rows {
        let (persona_id, persona_bytes) = row.map_err(authority_unavailable)?;
        budget.consume_row(&[persona_bytes], MAX_MEMORY_DELETION_FIELD_BYTES)?;
        personas.push(persona_id);
    }
    match personas.as_slice() {
        [] => Ok(None),
        [persona_id] => Ok(Some(persona_id.clone())),
        _ => Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        )),
    }
}

fn load_stored_event(
    connection: &Connection,
    persona_id: &str,
    deletion_id: &str,
    budget: &mut AuthorityReadBudget,
) -> Result<Option<StoredAuthorityEvent>, MemoryError> {
    let stored = connection
        .query_row(
            "SELECT authority_revision,
                    typeof(authority_revision) = 'integer',
                    CASE WHEN typeof(deletion_id) = 'text'
                              AND length(CAST(deletion_id AS BLOB)) <= ?3
                         THEN deletion_id END,
                    typeof(deletion_id) = 'text', length(CAST(deletion_id AS BLOB)),
                    CASE WHEN typeof(persona_id) = 'text'
                              AND length(CAST(persona_id AS BLOB)) <= ?3
                         THEN persona_id END,
                    typeof(persona_id) = 'text', length(CAST(persona_id AS BLOB)),
                    CASE WHEN typeof(recorded_at) = 'text'
                              AND length(CAST(recorded_at AS BLOB)) <= ?3
                         THEN recorded_at END,
                    typeof(recorded_at) = 'text', length(CAST(recorded_at AS BLOB)),
                    CASE WHEN typeof(durable_at) = 'text'
                              AND length(CAST(durable_at AS BLOB)) <= ?3
                         THEN durable_at END,
                    typeof(durable_at) = 'text', length(CAST(durable_at AS BLOB)),
                    CASE WHEN typeof(request_kind) = 'text'
                              AND length(CAST(request_kind AS BLOB)) <= ?3
                         THEN request_kind END,
                    request_kind IS NULL, typeof(request_kind) = 'text',
                    length(CAST(request_kind AS BLOB)),
                    CASE WHEN typeof(requested_memory_id) = 'text'
                              AND length(CAST(requested_memory_id AS BLOB)) <= ?3
                         THEN requested_memory_id END,
                    requested_memory_id IS NULL, typeof(requested_memory_id) = 'text',
                    length(CAST(requested_memory_id AS BLOB)),
                    target_memory_count, target_memory_count IS NULL,
                    typeof(target_memory_count) = 'integer',
                    CASE WHEN typeof(cleanup_completed_at) = 'text'
                              AND length(CAST(cleanup_completed_at AS BLOB)) <= ?3
                         THEN cleanup_completed_at END,
                    cleanup_completed_at IS NULL, typeof(cleanup_completed_at) = 'text',
                    length(CAST(cleanup_completed_at AS BLOB)),
                    deleted_memory_count, deleted_memory_count IS NULL,
                    typeof(deleted_memory_count) = 'integer',
                    CASE WHEN typeof(event_verifier) = 'blob'
                              AND length(CAST(event_verifier AS BLOB)) = 32
                         THEN event_verifier END,
                    typeof(event_verifier) = 'blob',
                    length(CAST(event_verifier AS BLOB))
             FROM deletion_event
             WHERE persona_id = ?1 AND deletion_id = ?2",
            params![
                persona_id,
                deletion_id,
                MAX_MEMORY_DELETION_FIELD_BYTES as i64
            ],
            |row| {
                let authority_revision = row.get::<_, i64>(0)?;
                if !row.get::<_, bool>(1)? {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let deletion = required_authority_text(
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let persona = required_authority_text(
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let recorded = required_authority_text(
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let durable = required_authority_text(
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let request_kind = optional_authority_text(
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let requested_memory = optional_authority_text(
                    row.get(18)?,
                    row.get(19)?,
                    row.get(20)?,
                    row.get(21)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let target_memory_count = row.get::<_, Option<i64>>(22)?;
                if row.get::<_, bool>(23)? != target_memory_count.is_none()
                    || (target_memory_count.is_some() && !row.get::<_, bool>(24)?)
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let cleanup = optional_authority_text(
                    row.get(25)?,
                    row.get(26)?,
                    row.get(27)?,
                    row.get(28)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let deleted_memory_count = row.get::<_, Option<i64>>(29)?;
                if row.get::<_, bool>(30)? != deleted_memory_count.is_none()
                    || (deleted_memory_count.is_some() && !row.get::<_, bool>(31)?)
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let verifier =
                    required_authority_blob(row.get(32)?, row.get(33)?, row.get(34)?, 32)?;
                Ok((
                    StoredAuthorityEvent {
                        authority_revision,
                        deletion_id: deletion.0,
                        persona_id: persona.0,
                        recorded_at: recorded.0,
                        durable_at: durable.0,
                        request_kind: request_kind.0,
                        requested_memory_id: requested_memory.0,
                        target_memory_count,
                        cleanup_completed_at: cleanup.0,
                        deleted_memory_count,
                        event_verifier: verifier.0,
                    },
                    [
                        deletion.1,
                        persona.1,
                        recorded.1,
                        durable.1,
                        request_kind.1,
                        requested_memory.1,
                        cleanup.1,
                        verifier.1,
                    ],
                ))
            },
        )
        .optional()
        .map_err(authority_unavailable)?;
    let Some((stored, field_bytes)) = stored else {
        return Ok(None);
    };
    budget.consume_row(&field_bytes, MAX_AUTHORITY_ROW_BYTES)?;
    Ok(Some(stored))
}

fn validate_stored_event(
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    event: &StoredAuthorityEvent,
) -> Result<(), MemoryError> {
    let expected_verifier = authority_event_verifier(derivation_key, event);
    if event.authority_revision <= 0
        || event.deletion_id.trim().is_empty()
        || event.persona_id.trim().is_empty()
        || chrono::DateTime::parse_from_rfc3339(&event.recorded_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&event.durable_at).is_err()
        || event.event_verifier.len() != expected_verifier.len()
        || !constant_time_equal(&event.event_verifier, &expected_verifier)
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    if !matches!(
        (
            event.request_kind.as_deref(),
            event.requested_memory_id.as_deref(),
            event.target_memory_count
        ),
        (None, None, None)
            | (Some("memory"), Some(_), Some(_))
            | (Some("persona_all"), None, Some(_))
    ) {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    if event
        .cleanup_completed_at
        .as_deref()
        .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_err())
        || (event.cleanup_completed_at.is_some()
            && (event.target_memory_count.is_none()
                || event.deleted_memory_count != event.target_memory_count))
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    Ok(())
}

fn load_subjects(
    authority_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
    persona_id: &str,
    deletion_id: &str,
    budget: &mut AuthorityReadBudget,
) -> Result<BTreeSet<MemoryDeletionSubject>, MemoryError> {
    let mut subjects = BTreeSet::new();
    let mut after_fingerprint: Option<Vec<u8>> = None;
    loop {
        let mut statement = connection
            .prepare(
                "SELECT
                    CASE WHEN typeof(subject_fingerprint) = 'blob'
                              AND length(CAST(subject_fingerprint AS BLOB)) = 32
                         THEN subject_fingerprint END,
                    typeof(subject_fingerprint) = 'blob',
                    length(CAST(subject_fingerprint AS BLOB)),
                    CASE WHEN typeof(subject_kind) = 'text'
                              AND length(CAST(subject_kind AS BLOB)) <= ?4
                         THEN subject_kind END,
                    typeof(subject_kind) = 'text', length(CAST(subject_kind AS BLOB)),
                    CASE WHEN typeof(memory_id) = 'text'
                              AND length(CAST(memory_id AS BLOB)) <= ?4
                         THEN memory_id END,
                    memory_id IS NULL, typeof(memory_id) = 'text',
                    length(CAST(memory_id AS BLOB)),
                    CASE WHEN typeof(conversation_id) = 'text'
                              AND length(CAST(conversation_id AS BLOB)) <= ?4
                         THEN conversation_id END,
                    conversation_id IS NULL, typeof(conversation_id) = 'text',
                    length(CAST(conversation_id AS BLOB)),
                    CASE WHEN typeof(turn_id) = 'text'
                              AND length(CAST(turn_id AS BLOB)) <= ?4
                         THEN turn_id END,
                    turn_id IS NULL, typeof(turn_id) = 'text',
                    length(CAST(turn_id AS BLOB)),
                    CASE WHEN typeof(derivation_key) = 'blob'
                              AND length(CAST(derivation_key AS BLOB)) = 32
                         THEN derivation_key END,
                    derivation_key IS NULL, typeof(derivation_key) = 'blob',
                    length(CAST(derivation_key AS BLOB))
                 FROM deletion_subject
                 WHERE persona_id = ?1 AND deletion_id = ?2
                   AND (?3 IS NULL OR subject_fingerprint > ?3)
                 ORDER BY subject_fingerprint
                 LIMIT ?5",
            )
            .map_err(authority_unavailable)?;
        let rows = statement
            .query_map(
                params![
                    persona_id,
                    deletion_id,
                    after_fingerprint.as_deref(),
                    MAX_MEMORY_DELETION_FIELD_BYTES as i64,
                    AUTHORITY_READ_BATCH_SIZE as i64
                ],
                |row| {
                    let fingerprint =
                        required_authority_blob(row.get(0)?, row.get(1)?, row.get(2)?, 32)?;
                    let kind = required_authority_text(
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let memory_id = optional_authority_text(
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let conversation_id = optional_authority_text(
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let turn_id = optional_authority_text(
                        row.get(14)?,
                        row.get(15)?,
                        row.get(16)?,
                        row.get(17)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let derivation_key = optional_authority_blob(
                        row.get(18)?,
                        row.get(19)?,
                        row.get(20)?,
                        row.get(21)?,
                        32,
                    )?;
                    Ok((
                        fingerprint,
                        kind,
                        memory_id,
                        conversation_id,
                        turn_id,
                        derivation_key,
                    ))
                },
            )
            .map_err(authority_unavailable)?;
        let mut batch = Vec::with_capacity(AUTHORITY_READ_BATCH_SIZE);
        for row in rows {
            let row = row.map_err(authority_unavailable)?;
            budget.consume_row(
                &[row.0.1, row.1.1, row.2.1, row.3.1, row.4.1, row.5.1],
                MAX_AUTHORITY_ROW_BYTES,
            )?;
            batch.push(row);
        }
        drop(statement);
        if batch.is_empty() {
            break;
        }
        for (stored_fingerprint, kind, memory_id, conversation_id, turn_id, derivation_key) in batch
        {
            let subject = match (
                kind.0.as_str(),
                memory_id.0,
                conversation_id.0,
                turn_id.0,
                derivation_key.0,
            ) {
                ("persona", None, None, None, None) => MemoryDeletionSubject::Persona {
                    persona_id: persona_id.to_string(),
                },
                ("memory", Some(memory_id), None, None, None) => MemoryDeletionSubject::Memory {
                    persona_id: persona_id.to_string(),
                    memory_id: crate::domain::memory::MemoryId(memory_id),
                },
                ("source_turn", None, Some(conversation_id), Some(turn_id), None) => {
                    MemoryDeletionSubject::SourceTurn {
                        persona_id: persona_id.to_string(),
                        conversation_id,
                        turn_id,
                    }
                }
                ("derivation", None, None, None, Some(bytes)) => {
                    let digest: [u8; MemoryDerivationKey::DIGEST_LENGTH] =
                        bytes.try_into().map_err(authority_unavailable)?;
                    MemoryDeletionSubject::Derivation {
                        persona_id: persona_id.to_string(),
                        derivation_key: MemoryDerivationKey::from_digest(digest),
                    }
                }
                _ => return Err(authority_unavailable_marker()),
            };
            subject
                .validate()
                .map_err(|_| authority_unavailable_marker())?;
            let expected_fingerprint = subject_fingerprint_with_key(authority_key, &subject);
            if !constant_time_equal(&stored_fingerprint.0, &expected_fingerprint) {
                return Err(authority_unavailable_marker());
            }
            after_fingerprint = Some(stored_fingerprint.0);
            if !subjects.insert(subject) || subjects.len() > MAX_MEMORY_DELETION_SUBJECTS {
                return Err(authority_limit_exceeded());
            }
        }
    }
    Ok(subjects)
}

fn configure_authority_connection(connection: &Connection) -> Result<(), MemoryError> {
    connection
        .busy_timeout(MEMORY_AUTHORITY_BUSY_TIMEOUT)
        .map_err(authority_unavailable)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(authority_unavailable)?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(authority_unavailable)?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(authority_unavailable)?;
    connection
        .pragma_update(None, "fullfsync", "ON")
        .map_err(authority_unavailable)?;
    Ok(())
}

fn install_authority_progress_handler(connection: &Connection) -> Arc<AtomicBool> {
    let callbacks = Arc::new(AtomicUsize::new(0));
    let exhausted = Arc::new(AtomicBool::new(false));
    let callback_count = Arc::clone(&callbacks);
    let callback_exhausted = Arc::clone(&exhausted);
    connection.progress_handler(
        AUTHORITY_SQLITE_PROGRESS_INTERVAL_OPS,
        Some(move || {
            let should_interrupt = callback_count.fetch_add(1, Ordering::Relaxed) + 1
                > MAX_AUTHORITY_SQLITE_PROGRESS_CALLBACKS;
            if should_interrupt {
                callback_exhausted.store(true, Ordering::Relaxed);
            }
            should_interrupt
        }),
    );
    exhausted
}

fn clear_authority_progress_handler(connection: &Connection) {
    connection.progress_handler(0, None::<fn() -> bool>);
}

fn open_validated_authority_connection(
    database_path: &Path,
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
) -> Result<(Connection, AuthorityReadBudget, Arc<AtomicBool>), MemoryError> {
    let connection = Connection::open(database_path).map_err(authority_unavailable)?;
    let exhausted = install_authority_progress_handler(&connection);
    let mut budget = AuthorityReadBudget::new();
    let result = (|| {
        configure_authority_connection(&connection)?;
        validate_authority_database(&connection, derivation_key, &mut budget)
    })();
    if let Err(error) = result {
        clear_authority_progress_handler(&connection);
        return Err(if exhausted.load(Ordering::Relaxed) {
            authority_limit_exceeded()
        } else {
            error
        });
    }
    Ok((connection, budget, exhausted))
}

fn load_runtime_authority_anchor(
    connection: &Connection,
    budget: &mut AuthorityReadBudget,
) -> Result<AuthorityAnchor, MemoryError> {
    let ((authority_id, authority_bytes), (initialized_at, initialized_bytes), revision) =
        connection
            .query_row(
                "SELECT
                    CASE WHEN typeof(authority_id) = 'text'
                              AND length(CAST(authority_id AS BLOB)) <= ?1
                         THEN authority_id END,
                    typeof(authority_id) = 'text',
                    length(CAST(authority_id AS BLOB)),
                    CASE WHEN typeof(initialized_at) = 'text'
                              AND length(CAST(initialized_at AS BLOB)) <= ?1
                         THEN initialized_at END,
                    typeof(initialized_at) = 'text',
                    length(CAST(initialized_at AS BLOB)),
                    last_applied_revision,
                    typeof(last_applied_revision) = 'integer'
                 FROM memory_authority_anchor
                 WHERE singleton = 1",
                [MAX_MEMORY_DELETION_FIELD_BYTES as i64],
                |row| {
                    let authority = required_authority_text(
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let initialized = required_authority_text(
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let revision = row.get::<_, i64>(6)?;
                    if !row.get::<_, bool>(7)? {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    Ok((authority, initialized, revision))
                },
            )
            .map_err(authority_unavailable)?;
    budget.consume_row(
        &[authority_bytes, initialized_bytes],
        MAX_MEMORY_DELETION_FIELD_BYTES * 2,
    )?;
    if authority_id.trim().is_empty()
        || chrono::DateTime::parse_from_rfc3339(&initialized_at).is_err()
        || revision < 0
    {
        return Err(authority_unavailable_marker());
    }
    Ok(AuthorityAnchor {
        authority_id,
        initialized_at,
        last_applied_revision: revision,
    })
}

fn load_authority_anchor(
    connection: &Connection,
    budget: &mut AuthorityReadBudget,
) -> Result<AuthorityAnchor, MemoryError> {
    let ((authority_id, authority_bytes), (initialized_at, initialized_bytes)) = connection
        .query_row(
            "SELECT
                CASE
                    WHEN typeof(authority_id) = 'text'
                     AND length(CAST(authority_id AS BLOB)) <= ?1
                    THEN authority_id
                END,
                typeof(authority_id) = 'text',
                length(CAST(authority_id AS BLOB)),
                CASE
                    WHEN typeof(created_at) = 'text'
                     AND length(CAST(created_at AS BLOB)) <= ?1
                    THEN created_at
                END,
                typeof(created_at) = 'text',
                length(CAST(created_at AS BLOB))
             FROM authority_meta
             WHERE singleton = 1",
            [MAX_MEMORY_DELETION_FIELD_BYTES as i64],
            |row| {
                let authority = required_authority_text(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                let initialized = required_authority_text(
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )?;
                Ok((authority, initialized))
            },
        )
        .map_err(authority_unavailable)?;
    budget.consume_row(
        &[authority_bytes, initialized_bytes],
        MAX_MEMORY_DELETION_FIELD_BYTES * 2,
    )?;
    if authority_id.trim().is_empty()
        || chrono::DateTime::parse_from_rfc3339(&initialized_at).is_err()
    {
        return Err(authority_unavailable_marker());
    }
    let last_applied_revision = connection
        .query_row(
            "SELECT COALESCE(MAX(authority_revision), 0) FROM deletion_event",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(authority_unavailable)?;
    if last_applied_revision < 0 {
        return Err(authority_unavailable_marker());
    }
    Ok(AuthorityAnchor {
        authority_id,
        initialized_at,
        last_applied_revision,
    })
}

fn required_authority_text(
    value: Option<String>,
    is_text: bool,
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<(String, usize), rusqlite::Error> {
    let byte_length = bounded_authority_length(byte_length, maximum_bytes)?;
    if !is_text {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let value = value.ok_or(rusqlite::Error::InvalidQuery)?;
    if value.len() != byte_length {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok((value, byte_length))
}

fn optional_authority_text(
    value: Option<String>,
    is_null: bool,
    is_text: bool,
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<(Option<String>, usize), rusqlite::Error> {
    if is_null {
        if value.is_some() || byte_length.is_some() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        return Ok((None, 0));
    }
    required_authority_text(value, is_text, byte_length, maximum_bytes)
        .map(|(value, bytes)| (Some(value), bytes))
}

fn required_authority_blob(
    value: Option<Vec<u8>>,
    is_blob: bool,
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<(Vec<u8>, usize), rusqlite::Error> {
    let byte_length = bounded_authority_length(byte_length, maximum_bytes)?;
    if !is_blob {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let value = value.ok_or(rusqlite::Error::InvalidQuery)?;
    if value.len() != byte_length {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok((value, byte_length))
}

fn optional_authority_blob(
    value: Option<Vec<u8>>,
    is_null: bool,
    is_blob: bool,
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<(Option<Vec<u8>>, usize), rusqlite::Error> {
    if is_null {
        if value.is_some() || byte_length.is_some() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        return Ok((None, 0));
    }
    required_authority_blob(value, is_blob, byte_length, maximum_bytes)
        .map(|(value, bytes)| (Some(value), bytes))
}

fn bounded_authority_length(
    byte_length: Option<i64>,
    maximum_bytes: usize,
) -> Result<usize, rusqlite::Error> {
    let byte_length = byte_length
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(rusqlite::Error::InvalidQuery)?;
    if byte_length > maximum_bytes {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(byte_length)
}

fn validate_authority_database(
    connection: &Connection,
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    budget: &mut AuthorityReadBudget,
) -> Result<(), MemoryError> {
    let (authority_id, schema_version, created_at, stored_verifier, stored_ledger, field_bytes) =
        connection
            .query_row(
                "SELECT
                CASE WHEN typeof(authority_id) = 'text'
                          AND length(CAST(authority_id AS BLOB)) <= ?1
                     THEN authority_id END,
                typeof(authority_id) = 'text', length(CAST(authority_id AS BLOB)),
                CASE WHEN typeof(schema_version) = 'text'
                          AND length(CAST(schema_version AS BLOB)) <= ?1
                     THEN schema_version END,
                typeof(schema_version) = 'text', length(CAST(schema_version AS BLOB)),
                CASE WHEN typeof(created_at) = 'text'
                          AND length(CAST(created_at AS BLOB)) <= ?1
                     THEN created_at END,
                typeof(created_at) = 'text', length(CAST(created_at AS BLOB)),
                CASE WHEN typeof(key_verifier) = 'blob'
                          AND length(CAST(key_verifier AS BLOB)) = 32
                     THEN key_verifier END,
                typeof(key_verifier) = 'blob', length(CAST(key_verifier AS BLOB)),
                CASE WHEN typeof(ledger_commitment) = 'blob'
                          AND length(CAST(ledger_commitment AS BLOB)) = 32
                     THEN ledger_commitment END,
                typeof(ledger_commitment) = 'blob',
                length(CAST(ledger_commitment AS BLOB))
             FROM authority_meta
             WHERE singleton = 1",
                [MAX_MEMORY_DELETION_FIELD_BYTES as i64],
                |row| {
                    let authority_id = required_authority_text(
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let schema_version = required_authority_text(
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let created_at = required_authority_text(
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let stored_verifier =
                        required_authority_blob(row.get(9)?, row.get(10)?, row.get(11)?, 32)?;
                    let stored_ledger =
                        required_authority_blob(row.get(12)?, row.get(13)?, row.get(14)?, 32)?;
                    Ok((
                        authority_id.0,
                        schema_version.0,
                        created_at.0,
                        stored_verifier.0,
                        stored_ledger.0,
                        [
                            authority_id.1,
                            schema_version.1,
                            created_at.1,
                            stored_verifier.1,
                            stored_ledger.1,
                        ],
                    ))
                },
            )
            .map_err(authority_unavailable)?;
    budget.consume_row(&field_bytes, MAX_AUTHORITY_ROW_BYTES)?;
    let expected_verifier = authority_key_verifier(derivation_key, &authority_id, &schema_version);
    if authority_id.trim().is_empty()
        || schema_version != MEMORY_AUTHORITY_SCHEMA_VERSION
        || chrono::DateTime::parse_from_rfc3339(&created_at).is_err()
        || stored_verifier.len() != expected_verifier.len()
        || !constant_time_equal(&stored_verifier, &expected_verifier)
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    for table in ["authority_meta", "deletion_event", "deletion_subject"] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = ?1
                 )",
                [table],
                |row| row.get(0),
            )
            .map_err(authority_unavailable)?;
        if !exists {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }
    }
    let (quick_check, quick_check_bytes) = connection
        .query_row(
            "SELECT CASE
                        WHEN typeof(quick_check) = 'text'
                         AND length(CAST(quick_check AS BLOB)) <= ?1
                        THEN quick_check
                    END,
                    typeof(quick_check) = 'text',
                    length(CAST(quick_check AS BLOB))
             FROM pragma_quick_check
             LIMIT 1",
            [MAX_MEMORY_DELETION_FIELD_BYTES as i64],
            |row| {
                required_authority_text(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    MAX_MEMORY_DELETION_FIELD_BYTES,
                )
            },
        )
        .map_err(authority_unavailable)?;
    budget.consume_row(&[quick_check_bytes], MAX_MEMORY_DELETION_FIELD_BYTES)?;
    if quick_check != "ok" {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    let mut foreign_key_check = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(authority_unavailable)?;
    if foreign_key_check
        .query([])
        .map_err(authority_unavailable)?
        .next()
        .map_err(authority_unavailable)?
        .is_some()
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    drop(foreign_key_check);
    let mut after_revision = 0_i64;
    loop {
        let mut events = connection
            .prepare(
                "SELECT authority_revision,
                        CASE WHEN typeof(persona_id) = 'text'
                                  AND length(CAST(persona_id AS BLOB)) <= ?2
                             THEN persona_id END,
                        typeof(persona_id) = 'text', length(CAST(persona_id AS BLOB)),
                        CASE WHEN typeof(deletion_id) = 'text'
                                  AND length(CAST(deletion_id AS BLOB)) <= ?2
                             THEN deletion_id END,
                        typeof(deletion_id) = 'text', length(CAST(deletion_id AS BLOB))
                 FROM deletion_event
                 WHERE authority_revision > ?1
                 ORDER BY authority_revision
                 LIMIT ?3",
            )
            .map_err(authority_unavailable)?;
        let rows = events
            .query_map(
                params![
                    after_revision,
                    MAX_MEMORY_DELETION_FIELD_BYTES as i64,
                    AUTHORITY_READ_BATCH_SIZE as i64
                ],
                |row| {
                    let revision = row.get::<_, i64>(0)?;
                    let persona = required_authority_text(
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let deletion = required_authority_text(
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    Ok((revision, persona, deletion))
                },
            )
            .map_err(authority_unavailable)?;
        let mut identities = Vec::with_capacity(AUTHORITY_READ_BATCH_SIZE);
        for row in rows {
            let (revision, persona, deletion) = row.map_err(authority_unavailable)?;
            if revision <= after_revision {
                return Err(authority_unavailable_marker());
            }
            budget.consume_row(
                &[persona.1, deletion.1],
                MAX_MEMORY_DELETION_FIELD_BYTES * 2,
            )?;
            identities.push((revision, persona.0, deletion.0));
        }
        drop(events);
        if identities.is_empty() {
            break;
        }
        for (revision, persona_id, deletion_id) in identities {
            let event = load_event_with_connection(
                derivation_key,
                connection,
                &persona_id,
                &deletion_id,
                budget,
            )?
            .ok_or_else(authority_unavailable_marker)?;
            if event.authority_revision != revision {
                return Err(authority_unavailable_marker());
            }
            after_revision = revision;
        }
    }
    let expected_ledger = authority_ledger_commitment(derivation_key, connection, budget)?;
    if stored_ledger.len() != expected_ledger.len()
        || !constant_time_equal(&stored_ledger, &expected_ledger)
    {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    Ok(())
}

fn insert_subject(
    connection: &Connection,
    deletion_id: &str,
    subject: &MemoryDeletionSubject,
    fingerprint: [u8; 32],
) -> Result<(), MemoryError> {
    let persona_id = subject_persona_id(subject);
    let (kind, memory_id, conversation_id, turn_id, derivation_key) = match subject {
        MemoryDeletionSubject::Persona { .. } => ("persona", None, None, None, None),
        MemoryDeletionSubject::Memory { memory_id, .. } => {
            ("memory", Some(memory_id.0.as_str()), None, None, None)
        }
        MemoryDeletionSubject::SourceTurn {
            conversation_id,
            turn_id,
            ..
        } => (
            "source_turn",
            None,
            Some(conversation_id.as_str()),
            Some(turn_id.as_str()),
            None,
        ),
        MemoryDeletionSubject::Derivation { derivation_key, .. } => (
            "derivation",
            None,
            None,
            None,
            Some(derivation_key.as_bytes().as_slice()),
        ),
    };
    connection
        .execute(
            "INSERT INTO deletion_subject(
                deletion_id, persona_id, subject_fingerprint, subject_kind,
                memory_id, conversation_id, turn_id, derivation_key
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                deletion_id,
                persona_id,
                fingerprint.as_slice(),
                kind,
                memory_id,
                conversation_id,
                turn_id,
                derivation_key
            ],
        )
        .map_err(authority_unavailable)?;
    Ok(())
}

fn subject_persona_id(subject: &MemoryDeletionSubject) -> &str {
    match subject {
        MemoryDeletionSubject::Persona { persona_id }
        | MemoryDeletionSubject::Memory { persona_id, .. }
        | MemoryDeletionSubject::SourceTurn { persona_id, .. }
        | MemoryDeletionSubject::Derivation { persona_id, .. } => persona_id,
    }
}

fn single_subject_persona(subjects: &BTreeSet<MemoryDeletionSubject>) -> Result<&str, MemoryError> {
    let mut personas = subjects.iter().map(subject_persona_id);
    let persona_id = personas
        .next()
        .ok_or_else(|| MemoryError::new(MemoryErrorCode::InvalidRequest))?;
    if personas.any(|candidate| candidate != persona_id) {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    Ok(persona_id)
}

fn append_length_prefixed(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

fn append_optional_text(target: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            target.push(1);
            append_length_prefixed(target, value.as_bytes());
        }
        None => target.push(0),
    }
}

fn append_optional_i64(target: &mut Vec<u8>, value: Option<i64>) {
    match value {
        Some(value) => {
            target.push(1);
            target.extend_from_slice(&value.to_be_bytes());
        }
        None => target.push(0),
    }
}

fn keyed_digest_with_key(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    domain: &[u8],
    fields: &[&[u8]],
) -> [u8; 32] {
    let capacity = fields
        .iter()
        .map(|field| field.len() + std::mem::size_of::<u64>())
        .sum::<usize>()
        + domain.len()
        + std::mem::size_of::<u64>();
    let mut material = Vec::with_capacity(capacity);
    append_length_prefixed(&mut material, domain);
    for field in fields {
        append_length_prefixed(&mut material, field);
    }
    hmac_sha256(key, &material)
}

fn subject_fingerprint_with_key(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    subject: &MemoryDeletionSubject,
) -> [u8; 32] {
    let persona_id = subject_persona_id(subject).as_bytes();
    match subject {
        MemoryDeletionSubject::Persona { .. } => keyed_digest_with_key(
            key,
            b"muse-memory-deletion-subject/v1",
            &[persona_id, b"persona"],
        ),
        MemoryDeletionSubject::Memory { memory_id, .. } => keyed_digest_with_key(
            key,
            b"muse-memory-deletion-subject/v1",
            &[persona_id, b"memory", memory_id.0.as_bytes()],
        ),
        MemoryDeletionSubject::SourceTurn {
            conversation_id,
            turn_id,
            ..
        } => keyed_digest_with_key(
            key,
            b"muse-memory-deletion-subject/v1",
            &[
                persona_id,
                b"source_turn",
                conversation_id.as_bytes(),
                turn_id.as_bytes(),
            ],
        ),
        MemoryDeletionSubject::Derivation { derivation_key, .. } => keyed_digest_with_key(
            key,
            b"muse-memory-deletion-subject/v1",
            &[persona_id, b"derivation", derivation_key.as_bytes()],
        ),
    }
}

fn authority_event_verifier(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    event: &StoredAuthorityEvent,
) -> [u8; 32] {
    let mut canonical = Vec::new();
    canonical.extend_from_slice(&event.authority_revision.to_be_bytes());
    append_length_prefixed(&mut canonical, event.deletion_id.as_bytes());
    append_length_prefixed(&mut canonical, event.persona_id.as_bytes());
    append_length_prefixed(&mut canonical, event.recorded_at.as_bytes());
    append_length_prefixed(&mut canonical, event.durable_at.as_bytes());
    append_optional_text(&mut canonical, event.request_kind.as_deref());
    append_optional_text(&mut canonical, event.requested_memory_id.as_deref());
    append_optional_i64(&mut canonical, event.target_memory_count);
    append_optional_text(&mut canonical, event.cleanup_completed_at.as_deref());
    append_optional_i64(&mut canonical, event.deleted_memory_count);
    keyed_digest_with_key(
        key,
        b"muse-memory-deletion-event/v1",
        &[canonical.as_slice()],
    )
}

fn authority_ledger_commitment(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
    budget: &mut AuthorityReadBudget,
) -> Result<[u8; 32], MemoryError> {
    let (event_count, event_bytes): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(
                        48
                        + length(CAST(deletion_id AS BLOB))
                        + length(CAST(persona_id AS BLOB))
                        + length(CAST(recorded_at AS BLOB))
                        + length(CAST(durable_at AS BLOB))
                    ), 0)
             FROM deletion_event
             WHERE typeof(deletion_id) = 'text'
               AND length(CAST(deletion_id AS BLOB)) <= ?1
               AND typeof(persona_id) = 'text'
               AND length(CAST(persona_id AS BLOB)) <= ?1
               AND typeof(recorded_at) = 'text'
               AND length(CAST(recorded_at AS BLOB)) <= ?1
               AND typeof(durable_at) = 'text'
               AND length(CAST(durable_at AS BLOB)) <= ?1",
            [MAX_MEMORY_DELETION_FIELD_BYTES as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(authority_unavailable)?;
    let total_event_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM deletion_event", [], |row| row.get(0))
        .map_err(authority_unavailable)?;
    if event_count != total_event_count {
        return Err(authority_unavailable_marker());
    }
    let (subject_count, subject_bytes): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(8 + length(CAST(subject_fingerprint AS BLOB))), 0)
             FROM deletion_subject
             WHERE typeof(subject_fingerprint) = 'blob'
               AND length(CAST(subject_fingerprint AS BLOB)) = 32",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(authority_unavailable)?;
    let total_subject_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM deletion_subject", [], |row| {
            row.get(0)
        })
        .map_err(authority_unavailable)?;
    if subject_count != total_subject_count {
        return Err(authority_unavailable_marker());
    }
    let event_count = usize::try_from(event_count).map_err(authority_unavailable)?;
    let subject_count = usize::try_from(subject_count).map_err(authority_unavailable)?;
    let authoritative_rows = event_count
        .checked_add(subject_count)
        .ok_or_else(authority_limit_exceeded)?;
    if authoritative_rows > MAX_AUTHORITY_ROWS {
        return Err(authority_limit_exceeded());
    }
    let canonical_length = 8_usize
        .checked_add(usize::try_from(event_bytes).map_err(authority_unavailable)?)
        .and_then(|value| value.checked_add(usize::try_from(subject_bytes).ok()?))
        .ok_or_else(authority_limit_exceeded)?;
    if canonical_length > MAX_AUTHORITY_MATERIALIZED_BYTES {
        return Err(authority_limit_exceeded());
    }

    let mut digest = StreamingHmacSha256::new(key);
    digest.update_length_prefixed(b"muse-memory-deletion-ledger/v1")?;
    digest.update_u64(canonical_length)?;
    digest.update_u64(event_count)?;

    let mut after_revision = 0_i64;
    loop {
        let mut statement = connection
            .prepare(
                "SELECT authority_revision,
                        CASE WHEN typeof(deletion_id) = 'text'
                                  AND length(CAST(deletion_id AS BLOB)) <= ?2
                             THEN deletion_id END,
                        typeof(deletion_id) = 'text', length(CAST(deletion_id AS BLOB)),
                        CASE WHEN typeof(persona_id) = 'text'
                                  AND length(CAST(persona_id AS BLOB)) <= ?2
                             THEN persona_id END,
                        typeof(persona_id) = 'text', length(CAST(persona_id AS BLOB)),
                        CASE WHEN typeof(recorded_at) = 'text'
                                  AND length(CAST(recorded_at AS BLOB)) <= ?2
                             THEN recorded_at END,
                        typeof(recorded_at) = 'text', length(CAST(recorded_at AS BLOB)),
                        CASE WHEN typeof(durable_at) = 'text'
                                  AND length(CAST(durable_at AS BLOB)) <= ?2
                             THEN durable_at END,
                        typeof(durable_at) = 'text', length(CAST(durable_at AS BLOB))
                 FROM deletion_event
                 WHERE authority_revision > ?1
                 ORDER BY authority_revision
                 LIMIT ?3",
            )
            .map_err(authority_unavailable)?;
        let rows = statement
            .query_map(
                params![
                    after_revision,
                    MAX_MEMORY_DELETION_FIELD_BYTES as i64,
                    AUTHORITY_READ_BATCH_SIZE as i64
                ],
                |row| {
                    let deletion = required_authority_text(
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let persona = required_authority_text(
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let recorded = required_authority_text(
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    let durable = required_authority_text(
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        MAX_MEMORY_DELETION_FIELD_BYTES,
                    )?;
                    Ok((row.get::<_, i64>(0)?, deletion, persona, recorded, durable))
                },
            )
            .map_err(authority_unavailable)?;
        let mut batch = Vec::with_capacity(AUTHORITY_READ_BATCH_SIZE);
        for row in rows {
            let row = row.map_err(authority_unavailable)?;
            budget.consume_row(
                &[row.1.1, row.2.1, row.3.1, row.4.1],
                MAX_AUTHORITY_ROW_BYTES,
            )?;
            batch.push(row);
        }
        drop(statement);
        if batch.is_empty() {
            break;
        }
        for (revision, deletion_id, persona_id, recorded_at, durable_at) in batch {
            if revision <= after_revision {
                return Err(authority_unavailable_marker());
            }
            digest.update(&revision.to_be_bytes());
            digest.update_length_prefixed(deletion_id.0.as_bytes())?;
            digest.update_length_prefixed(persona_id.0.as_bytes())?;
            digest.update_length_prefixed(recorded_at.0.as_bytes())?;
            digest.update_length_prefixed(durable_at.0.as_bytes())?;

            let per_event_count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM deletion_subject
                     WHERE persona_id = ?1 AND deletion_id = ?2",
                    params![persona_id.0, deletion_id.0],
                    |row| row.get(0),
                )
                .map_err(authority_unavailable)?;
            let per_event_count =
                usize::try_from(per_event_count).map_err(authority_unavailable)?;
            if per_event_count > MAX_MEMORY_DELETION_SUBJECTS {
                return Err(authority_limit_exceeded());
            }
            digest.update_u64(per_event_count)?;
            let mut after_fingerprint: Option<Vec<u8>> = None;
            let mut streamed = 0_usize;
            loop {
                let mut subjects = connection
                    .prepare(
                        "SELECT CASE
                                    WHEN typeof(subject_fingerprint) = 'blob'
                                     AND length(CAST(subject_fingerprint AS BLOB)) = 32
                                    THEN subject_fingerprint
                                END,
                                typeof(subject_fingerprint) = 'blob',
                                length(CAST(subject_fingerprint AS BLOB))
                         FROM deletion_subject
                         WHERE persona_id = ?1 AND deletion_id = ?2
                           AND (?3 IS NULL OR subject_fingerprint > ?3)
                         ORDER BY subject_fingerprint
                         LIMIT ?4",
                    )
                    .map_err(authority_unavailable)?;
                let rows = subjects
                    .query_map(
                        params![
                            persona_id.0,
                            deletion_id.0,
                            after_fingerprint.as_deref(),
                            AUTHORITY_READ_BATCH_SIZE as i64
                        ],
                        |row| required_authority_blob(row.get(0)?, row.get(1)?, row.get(2)?, 32),
                    )
                    .map_err(authority_unavailable)?;
                let mut fingerprints = Vec::with_capacity(AUTHORITY_READ_BATCH_SIZE);
                for row in rows {
                    let fingerprint = row.map_err(authority_unavailable)?;
                    budget.consume_row(&[fingerprint.1], 32)?;
                    fingerprints.push(fingerprint.0);
                }
                drop(subjects);
                if fingerprints.is_empty() {
                    break;
                }
                for fingerprint in fingerprints {
                    digest.update_length_prefixed(&fingerprint)?;
                    after_fingerprint = Some(fingerprint);
                    streamed = streamed
                        .checked_add(1)
                        .ok_or_else(authority_limit_exceeded)?;
                }
            }
            if streamed != per_event_count {
                return Err(authority_unavailable_marker());
            }
            after_revision = revision;
        }
    }
    Ok(digest.finalize())
}

fn refresh_ledger_commitment(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
    budget: &mut AuthorityReadBudget,
) -> Result<(), MemoryError> {
    let commitment = authority_ledger_commitment(key, connection, budget)?;
    let updated = connection
        .execute(
            "UPDATE authority_meta
             SET ledger_commitment = ?1
             WHERE singleton = 1",
            [commitment.as_slice()],
        )
        .map_err(authority_unavailable)?;
    if updated != 1 {
        return Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ));
    }
    Ok(())
}

fn hmac_sha256(key: &[u8; 32], material: &[u8]) -> [u8; 32] {
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in key.iter().copied().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(material);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

struct StreamingHmacSha256 {
    inner: Sha256,
    outer_pad: [u8; 64],
}

impl StreamingHmacSha256 {
    fn new(key: &[u8; 32]) -> Self {
        let mut inner_pad = [0x36_u8; 64];
        let mut outer_pad = [0x5c_u8; 64];
        for (index, byte) in key.iter().copied().enumerate() {
            inner_pad[index] ^= byte;
            outer_pad[index] ^= byte;
        }
        let mut inner = Sha256::new();
        inner.update(inner_pad);
        Self { inner, outer_pad }
    }

    fn update(&mut self, value: &[u8]) {
        self.inner.update(value);
    }

    fn update_u64(&mut self, value: usize) -> Result<(), MemoryError> {
        let value = u64::try_from(value).map_err(authority_unavailable)?;
        self.update(&value.to_be_bytes());
        Ok(())
    }

    fn update_length_prefixed(&mut self, value: &[u8]) -> Result<(), MemoryError> {
        self.update_u64(value.len())?;
        self.update(value);
        Ok(())
    }

    fn finalize(self) -> [u8; 32] {
        let inner_digest = self.inner.finalize();
        let mut outer = Sha256::new();
        outer.update(self.outer_pad);
        outer.update(inner_digest);
        outer.finalize().into()
    }
}

fn authority_key_verifier(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    authority_id: &str,
    schema_version: &str,
) -> [u8; 32] {
    let mut material = Vec::new();
    append_length_prefixed(&mut material, b"muse-memory-authority-key-verifier/v1");
    append_length_prefixed(&mut material, authority_id.as_bytes());
    append_length_prefixed(&mut material, schema_version.as_bytes());
    hmac_sha256(key, &material)
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
        && left.len() == right.len()
}

fn publish_new_derivation_key(
    path: &Path,
    privacy_dir: &Path,
) -> Result<[u8; MEMORY_DERIVATION_KEY_LENGTH], MemoryError> {
    let mut key = [0_u8; MEMORY_DERIVATION_KEY_LENGTH];
    getrandom::fill(&mut key).map_err(authority_unavailable)?;
    atomic_write_sensitive_synced(path, &key).map_err(authority_unavailable)?;
    sync_parent_directory_required(privacy_dir).map_err(authority_unavailable)?;
    Ok(key)
}

fn initialize_authority_database(
    database_path: &Path,
    privacy_dir: &Path,
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
) -> Result<(), MemoryError> {
    let temporary = create_unique_temporary_path(database_path).map_err(authority_unavailable)?;
    let result = (|| -> Result<(), MemoryError> {
        restrict_sensitive_file_permissions(&temporary).map_err(authority_unavailable)?;
        let connection = Connection::open(&temporary).map_err(authority_unavailable)?;
        let exhausted = install_authority_progress_handler(&connection);
        let mut budget = AuthorityReadBudget::new();
        let initialize_result = (|| {
            configure_authority_connection(&connection)?;
            let initialized_at = Utc::now().to_rfc3339();
            let authority_id = new_authority_id()?;
            let verifier = authority_key_verifier(
                derivation_key,
                &authority_id,
                MEMORY_AUTHORITY_SCHEMA_VERSION,
            );
            connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(authority_unavailable)?;
            connection
                .execute_batch(MEMORY_AUTHORITY_SCHEMA)
                .map_err(authority_unavailable)?;
            let ledger_commitment =
                authority_ledger_commitment(derivation_key, &connection, &mut budget)?;
            connection
                .execute(
                    "INSERT INTO authority_meta(
                        singleton, authority_id, schema_version, created_at,
                        key_verifier, ledger_commitment
                     ) VALUES(1, ?1, ?2, ?3, ?4, ?5)",
                    params![
                        authority_id,
                        MEMORY_AUTHORITY_SCHEMA_VERSION,
                        initialized_at,
                        verifier.as_slice(),
                        ledger_commitment.as_slice()
                    ],
                )
                .map_err(authority_unavailable)?;
            connection
                .execute_batch("COMMIT")
                .map_err(authority_unavailable)?;
            validate_authority_database(&connection, derivation_key, &mut budget)
        })();
        clear_authority_progress_handler(&connection);
        if exhausted.load(Ordering::Relaxed) {
            return Err(authority_limit_exceeded());
        }
        initialize_result?;
        drop(connection);
        sync_authority_file(&temporary)?;
        replace_file(&temporary, database_path).map_err(authority_unavailable)?;
        restrict_sensitive_file_permissions(database_path).map_err(authority_unavailable)?;
        sync_parent_directory_required(privacy_dir).map_err(authority_unavailable)?;
        sync_authority_file(database_path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn regular_file_length(path: &Path) -> Result<Option<u64>, MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable),
        ),
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(authority_unavailable(error)),
    }
}

fn regular_file_exists(path: &Path) -> Result<bool, MemoryError> {
    regular_file_length(path).map(|length| length.is_some())
}

fn read_derivation_key(path: &Path) -> Result<[u8; MEMORY_DERIVATION_KEY_LENGTH], MemoryError> {
    let bytes = fs::read(path).map_err(authority_unavailable)?;
    bytes
        .try_into()
        .map_err(|_| MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable))
}

fn new_authority_id() -> Result<String, MemoryError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(authority_unavailable)?;
    let mut encoded = String::with_capacity("memory-authority-".len() + random.len() * 2);
    encoded.push_str("memory-authority-");
    for byte in random {
        use fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
    }
    Ok(encoded)
}

#[cfg(not(test))]
fn observe_authority_commit_window(_path: &Path) -> Result<(), MemoryError> {
    Ok(())
}

#[cfg(test)]
fn observe_authority_commit_window(path: &Path) -> Result<(), MemoryError> {
    let hook = {
        let mut configured = PAUSE_AFTER_AUTHORITY_COMMIT
            .lock()
            .map_err(authority_unavailable)?;
        if configured
            .as_ref()
            .is_some_and(|hook| hook.database_path == path)
        {
            configured.take()
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        hook.entered.send(()).map_err(authority_unavailable)?;
        hook.resume.recv().map_err(authority_unavailable)?;
    }
    Ok(())
}

fn sync_authority_file(path: &Path) -> Result<(), MemoryError> {
    #[cfg(test)]
    {
        let mut injection = FAIL_NEXT_AUTHORITY_SYNCS
            .lock()
            .map_err(authority_unavailable)?;
        if let Some((target, remaining)) = injection.as_mut()
            && target == path
            && *remaining > 0
        {
            *remaining -= 1;
            if *remaining == 0 {
                *injection = None;
            }
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(authority_unavailable)?;
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable))?;
    sync_parent_directory_required(parent).map_err(authority_unavailable)
}

#[cfg(test)]
pub(crate) fn fail_next_authority_syncs_for_test(path: PathBuf, failures: usize) {
    assert!(failures > 0, "故障注入次数必须大于零");
    *FAIL_NEXT_AUTHORITY_SYNCS
        .lock()
        .expect("测试故障注入锁不应中毒") = Some((path, failures));
}

#[cfg(test)]
pub(crate) fn pause_after_authority_commit_for_test(
    path: PathBuf,
) -> (Receiver<()>, SyncSender<()>) {
    let (entered_sender, entered_receiver) = sync_channel(0);
    let (resume_sender, resume_receiver) = sync_channel(0);
    let mut hook = PAUSE_AFTER_AUTHORITY_COMMIT
        .lock()
        .expect("authority commit 窗口测试锁不应中毒");
    assert!(
        hook.is_none(),
        "同一时刻只允许一个 authority commit 窗口测试"
    );
    *hook = Some(AuthorityCommitWindowHook {
        database_path: path,
        entered: entered_sender,
        resume: resume_receiver,
    });
    (entered_receiver, resume_sender)
}

fn authority_revision(revision: i64) -> String {
    format!("memory-deletion-authority/{revision}")
}

fn authority_unavailable<T>(_error: T) -> MemoryError {
    MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable)
}

fn authority_unavailable_marker() -> MemoryError {
    MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable)
}

fn authority_limit_exceeded() -> MemoryError {
    MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable)
}

#[cfg(test)]
mod resource_tests {
    use super::*;

    fn ledger_connection() -> Connection {
        let connection = Connection::open_in_memory().expect("应打开 authority 测试数据库");
        connection
            .execute_batch(MEMORY_AUTHORITY_SCHEMA)
            .expect("应建立真实 authority schema");
        connection
    }

    fn padded_field(prefix: &str, index: usize) -> String {
        let prefix = format!("{prefix}-{index:04}-");
        assert!(prefix.len() < MAX_MEMORY_DELETION_FIELD_BYTES);
        format!(
            "{prefix}{}",
            "x".repeat(MAX_MEMORY_DELETION_FIELD_BYTES - prefix.len())
        )
    }

    #[test]
    fn authority_ledger流式_hmac保持既有canonical字节兼容() {
        let connection = ledger_connection();
        let verifier = [0_u8; 32];
        connection
            .execute(
                "INSERT INTO deletion_event(
                    deletion_id, persona_id, recorded_at, durable_at, event_verifier
                 ) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    "deletion-1",
                    "persona-1",
                    TIME_FOR_TEST,
                    TIME_FOR_TEST,
                    verifier
                ],
            )
            .expect("应插入 ledger 事件");
        let fingerprints = [[1_u8; 32], [2_u8; 32]];
        for (index, fingerprint) in fingerprints.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO deletion_subject(
                        deletion_id, persona_id, subject_fingerprint, subject_kind, memory_id
                     ) VALUES(?1, ?2, ?3, 'memory', ?4)",
                    params![
                        "deletion-1",
                        "persona-1",
                        fingerprint,
                        format!("memory-{index}")
                    ],
                )
                .expect("应插入 ledger fingerprint");
        }

        let mut canonical = Vec::new();
        canonical.extend_from_slice(&1_u64.to_be_bytes());
        canonical.extend_from_slice(&1_i64.to_be_bytes());
        for value in ["deletion-1", "persona-1", TIME_FOR_TEST, TIME_FOR_TEST] {
            append_length_prefixed(&mut canonical, value.as_bytes());
        }
        canonical.extend_from_slice(&2_u64.to_be_bytes());
        for fingerprint in fingerprints {
            append_length_prefixed(&mut canonical, &fingerprint);
        }
        let key = [0x5a_u8; 32];
        let expected = keyed_digest_with_key(
            &key,
            b"muse-memory-deletion-ledger/v1",
            &[canonical.as_slice()],
        );
        let actual =
            authority_ledger_commitment(&key, &connection, &mut AuthorityReadBudget::new())
                .expect("流式 ledger 应可计算");
        assert_eq!(actual, expected);
    }

    #[test]
    fn authority_ledger拒绝513个subject_fingerprint() {
        let mut connection = ledger_connection();
        let transaction = connection.transaction().expect("应开启 subject 压力事务");
        let verifier = [0_u8; 32];
        transaction
            .execute(
                "INSERT INTO deletion_event(
                    deletion_id, persona_id, recorded_at, durable_at, event_verifier
                 ) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    "deletion-513",
                    "persona-513",
                    TIME_FOR_TEST,
                    TIME_FOR_TEST,
                    verifier
                ],
            )
            .expect("应插入 subject 压力事件");
        for index in 0..=MAX_MEMORY_DELETION_SUBJECTS {
            let mut fingerprint = [0_u8; 32];
            fingerprint[..8].copy_from_slice(&(index as u64).to_be_bytes());
            transaction
                .execute(
                    "INSERT INTO deletion_subject(
                        deletion_id, persona_id, subject_fingerprint, subject_kind, memory_id
                     ) VALUES(?1, ?2, ?3, 'memory', ?4)",
                    params![
                        "deletion-513",
                        "persona-513",
                        fingerprint,
                        format!("memory-{index:04}")
                    ],
                )
                .expect("应插入真实 fingerprint 行");
        }
        transaction.commit().expect("subject 压力夹具应提交");

        assert_eq!(
            authority_ledger_commitment(
                &[0x5a_u8; 32],
                &connection,
                &mut AuthorityReadBudget::new(),
            )
            .expect_err("单事件第 513 个 fingerprint 必须拒绝")
            .code(),
            MemoryErrorCode::DeletionAuthorityUnavailable
        );
    }

    #[test]
    fn authority_ledger对事件行数和累计字节均有硬上限() {
        let mut row_connection = ledger_connection();
        let transaction = row_connection.transaction().expect("应开启事件行压力事务");
        let verifier = [0_u8; 32];
        for index in 0..=MAX_AUTHORITY_ROWS {
            transaction
                .execute(
                    "INSERT INTO deletion_event(
                        deletion_id, persona_id, recorded_at, durable_at, event_verifier
                     ) VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        format!("deletion-{index:05}"),
                        format!("persona-{index:05}"),
                        TIME_FOR_TEST,
                        TIME_FOR_TEST,
                        verifier
                    ],
                )
                .expect("应插入真实 event 行");
        }
        transaction.commit().expect("事件行压力夹具应提交");
        assert_eq!(
            authority_ledger_commitment(
                &[0x5a_u8; 32],
                &row_connection,
                &mut AuthorityReadBudget::new(),
            )
            .expect_err("authority event 行数超过上限时必须拒绝")
            .code(),
            MemoryErrorCode::DeletionAuthorityUnavailable
        );

        let mut byte_connection = ledger_connection();
        let transaction = byte_connection
            .transaction()
            .expect("应开启累计字节压力事务");
        let per_event_bytes = 48 + MAX_MEMORY_DELETION_FIELD_BYTES * 4;
        let event_count = (MAX_AUTHORITY_MATERIALIZED_BYTES - 8) / per_event_bytes + 1;
        for index in 0..event_count {
            transaction
                .execute(
                    "INSERT INTO deletion_event(
                        deletion_id, persona_id, recorded_at, durable_at, event_verifier
                     ) VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![
                        padded_field("deletion", index),
                        padded_field("persona", index),
                        padded_field("recorded", index),
                        padded_field("durable", index),
                        verifier
                    ],
                )
                .expect("应插入累计字节压力事件");
        }
        transaction.commit().expect("累计字节压力夹具应提交");
        assert_eq!(
            authority_ledger_commitment(
                &[0x5a_u8; 32],
                &byte_connection,
                &mut AuthorityReadBudget::new(),
            )
            .expect_err("authority ledger 累计字节超过上限时必须拒绝")
            .code(),
            MemoryErrorCode::DeletionAuthorityUnavailable
        );
    }

    const TIME_FOR_TEST: &str = "2026-08-02T00:00:00Z";
}
