//! Persona 长期记忆的独立删除权威。
//!
//! 普通记忆正文属于 `runtime/muse.sqlite` 的备份域；前向删除权威与派生密钥
//! 位于独立 `privacy/` 恢复域，恢复旧主库时不得被普通备份覆盖。

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
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
    MemoryDeletionAuthority, MemoryDeletionAuthorityReceipt, MemoryDeletionAuthorityRequest,
    MemoryDeletionCheckRequest, MemoryDeletionDecision, MemoryDeletionSubject, MemoryDerivationKey,
    MemoryError, MemoryErrorCode,
};

const MEMORY_PRIVACY_DIRECTORY: &str = "privacy";
const MEMORY_DELETION_AUTHORITY_FILE: &str = "memory-deletion-authority.sqlite";
const MEMORY_DERIVATION_KEY_FILE: &str = "memory-derivation.key";
const MEMORY_DERIVATION_KEY_LENGTH: usize = 32;
const MEMORY_AUTHORITY_SCHEMA_VERSION: &str = "muse-memory-deletion-authority/v1";
const MEMORY_AUTHORITY_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
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
    finished: bool,
}

impl CanonicalAuthorityGuard<'_> {
    pub(crate) fn check(
        &self,
        request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        check_with_connection(self.authority, &self.connection, request)
    }

    pub(crate) fn event(
        &self,
        persona_id: &str,
        deletion_id: &str,
    ) -> Result<Option<AuthorityDeletionEvent>, MemoryError> {
        load_event_with_connection(
            &self.authority.derivation_key,
            &self.connection,
            persona_id,
            deletion_id,
        )
    }

    /// confirmation ID 在整个删除权威中全局唯一；跨 Persona 复用必须稳定拒绝，
    /// 不能因旧 schema 的复合唯一键而被当成另一条独立确认。
    pub(crate) fn event_by_deletion_id(
        &self,
        deletion_id: &str,
    ) -> Result<Option<AuthorityDeletionEvent>, MemoryError> {
        let Some(persona_id) = event_persona_by_deletion_id(&self.connection, deletion_id)? else {
            return Ok(None);
        };
        load_event_with_connection(
            &self.authority.derivation_key,
            &self.connection,
            &persona_id,
            deletion_id,
        )
    }

    pub(crate) fn pending_events(
        &self,
        last_applied_revision: i64,
    ) -> Result<Vec<AuthorityDeletionEvent>, MemoryError> {
        load_pending_events(
            &self.authority.derivation_key,
            &self.connection,
            last_applied_revision,
        )
    }

    pub(crate) fn max_revision(&self) -> Result<i64, MemoryError> {
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
            self.authority,
            &self.connection,
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
            self.authority,
            &self.connection,
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
        &self,
        persona_id: &str,
        deletion_id: &str,
        deleted_memory_count: u64,
        completed_at: &str,
    ) -> Result<(), MemoryError> {
        let mut existing = load_stored_event(&self.connection, persona_id, deletion_id)?
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
        self.connection
            .execute_batch("COMMIT")
            .map_err(authority_unavailable)?;
        self.finished = true;
        Ok(())
    }

    pub(crate) fn finish_durable(mut self) -> Result<(), MemoryError> {
        self.connection
            .execute_batch("COMMIT")
            .map_err(authority_unavailable)?;
        sync_authority_file(&self.authority.database_path)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for CanonicalAuthorityGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
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
        let anchor = connection
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
            .map_err(authority_unavailable)?;
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
            let connection = Connection::open(&database_path).map_err(authority_unavailable)?;
            restrict_sensitive_file_permissions(&database_path).map_err(authority_unavailable)?;
            configure_authority_connection(&connection)?;
            validate_authority_database(&connection, &derivation_key)?;
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
        let connection = Connection::open(&database_path).map_err(authority_unavailable)?;
        restrict_sensitive_file_permissions(&database_path).map_err(authority_unavailable)?;
        configure_authority_connection(&connection)?;
        validate_authority_database(&connection, &derivation_key)?;
        drop(connection);
        sync_authority_file(&database_path)?;

        Ok(Self {
            database_path,
            derivation_key,
        })
    }

    pub(crate) fn anchor(&self) -> Result<AuthorityAnchor, MemoryError> {
        let connection = self.open_validated_connection()?;
        let (authority_id, initialized_at) = connection
            .query_row(
                "SELECT authority_id, created_at
                 FROM authority_meta
                 WHERE singleton = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(authority_unavailable)?;
        let last_applied_revision = connection
            .query_row(
                "SELECT COALESCE(MAX(authority_revision), 0)
                 FROM deletion_event",
                [],
                |row| row.get(0),
            )
            .map_err(authority_unavailable)?;
        Ok(AuthorityAnchor {
            authority_id,
            initialized_at,
            last_applied_revision,
        })
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

    fn open_validated_connection(&self) -> Result<Connection, MemoryError> {
        let connection = Connection::open(&self.database_path).map_err(authority_unavailable)?;
        configure_authority_connection(&connection)?;
        validate_authority_database(&connection, &self.derivation_key)?;
        Ok(connection)
    }

    pub(crate) fn begin_guard(&self) -> Result<CanonicalAuthorityGuard<'_>, MemoryError> {
        let connection = self.open_validated_connection()?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(authority_unavailable)?;
        Ok(CanonicalAuthorityGuard {
            authority: self,
            connection,
            finished: false,
        })
    }
}

fn upsert_event_in_transaction(
    authority: &SqliteMemoryDeletionAuthority,
    connection: &Connection,
    persona_id: &str,
    deletion_id: &str,
    recorded_at: &str,
    subjects: &BTreeSet<MemoryDeletionSubject>,
    intent: Option<(&str, Option<&str>, u64)>,
) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
    if subjects
        .iter()
        .any(|subject| subject_persona_id(subject) != persona_id)
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    let requested_intent = intent
        .map(|(kind, memory_id, count)| {
            i64::try_from(count)
                .map(|count| (kind, memory_id, count))
                .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))
        })
        .transpose()?;
    if event_persona_by_deletion_id(connection, deletion_id)?
        .is_some_and(|stored_persona| stored_persona != persona_id)
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    if let Some(mut existing) = load_stored_event(connection, persona_id, deletion_id)? {
        validate_stored_event(&authority.derivation_key, &existing)?;
        let stored_subjects = load_subjects(
            &authority.derivation_key,
            connection,
            persona_id,
            deletion_id,
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
    refresh_ledger_commitment(&authority.derivation_key, connection)?;
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
        let guard = self.begin_guard()?;
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
) -> Result<Vec<AuthorityDeletionEvent>, MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT persona_id, deletion_id
             FROM deletion_event
             WHERE authority_revision > ?1
                OR (request_kind IS NOT NULL AND cleanup_completed_at IS NULL)
             ORDER BY authority_revision",
        )
        .map_err(authority_unavailable)?;
    let identities = statement
        .query_map([last_applied_revision], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(authority_unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(authority_unavailable)?;
    drop(statement);

    identities
        .into_iter()
        .map(|(persona_id, deletion_id)| {
            load_event_with_connection(derivation_key, connection, &persona_id, &deletion_id)?
                .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable))
        })
        .collect()
}

fn load_event_with_connection(
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
    persona_id: &str,
    deletion_id: &str,
) -> Result<Option<AuthorityDeletionEvent>, MemoryError> {
    let Some(stored) = load_stored_event(connection, persona_id, deletion_id)? else {
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
    let subjects = load_subjects(derivation_key, connection, persona_id, deletion_id)?;
    Ok(Some(AuthorityDeletionEvent {
        authority_revision: stored.authority_revision,
        deletion_id: stored.deletion_id,
        persona_id: stored.persona_id,
        recorded_at: stored.recorded_at,
        subjects,
        request_kind: stored.request_kind,
        requested_memory_id: stored.requested_memory_id,
        target_memory_count,
        cleanup_completed_at: stored.cleanup_completed_at,
    }))
}

fn event_persona_by_deletion_id(
    connection: &Connection,
    deletion_id: &str,
) -> Result<Option<String>, MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT persona_id
             FROM deletion_event
             WHERE deletion_id = ?1
             ORDER BY authority_revision
             LIMIT 2",
        )
        .map_err(authority_unavailable)?;
    let personas = statement
        .query_map([deletion_id], |row| row.get::<_, String>(0))
        .map_err(authority_unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(authority_unavailable)?;
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
) -> Result<Option<StoredAuthorityEvent>, MemoryError> {
    connection
        .query_row(
            "SELECT authority_revision, recorded_at, durable_at,
                    request_kind, requested_memory_id, target_memory_count,
                    cleanup_completed_at, deleted_memory_count, event_verifier
             FROM deletion_event
             WHERE persona_id = ?1 AND deletion_id = ?2",
            params![persona_id, deletion_id],
            |row| {
                Ok(StoredAuthorityEvent {
                    authority_revision: row.get(0)?,
                    deletion_id: deletion_id.to_string(),
                    persona_id: persona_id.to_string(),
                    recorded_at: row.get(1)?,
                    durable_at: row.get(2)?,
                    request_kind: row.get(3)?,
                    requested_memory_id: row.get(4)?,
                    target_memory_count: row.get(5)?,
                    cleanup_completed_at: row.get(6)?,
                    deleted_memory_count: row.get(7)?,
                    event_verifier: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(authority_unavailable)
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
) -> Result<BTreeSet<MemoryDeletionSubject>, MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT subject_fingerprint, subject_kind, memory_id,
                    conversation_id, turn_id, derivation_key
             FROM deletion_subject
             WHERE persona_id = ?1 AND deletion_id = ?2
             ORDER BY subject_fingerprint",
        )
        .map_err(authority_unavailable)?;
    let rows = statement
        .query_map(params![persona_id, deletion_id], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
            ))
        })
        .map_err(authority_unavailable)?;
    let mut subjects = BTreeSet::new();
    for row in rows {
        let (stored_fingerprint, kind, memory_id, conversation_id, turn_id, derivation_key) =
            row.map_err(authority_unavailable)?;
        let subject = match (
            kind.as_str(),
            memory_id,
            conversation_id,
            turn_id,
            derivation_key,
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
            _ => {
                return Err(MemoryError::new(
                    MemoryErrorCode::DeletionAuthorityUnavailable,
                ));
            }
        };
        let expected_fingerprint = subject_fingerprint_with_key(authority_key, &subject);
        if stored_fingerprint.len() != expected_fingerprint.len()
            || !constant_time_equal(&stored_fingerprint, &expected_fingerprint)
        {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }
        subjects.insert(subject);
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

fn validate_authority_database(
    connection: &Connection,
    derivation_key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
) -> Result<(), MemoryError> {
    let (authority_id, schema_version, stored_verifier, stored_ledger) = connection
        .query_row(
            "SELECT authority_id, schema_version, key_verifier, ledger_commitment
             FROM authority_meta
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .map_err(authority_unavailable)?;
    let expected_verifier = authority_key_verifier(derivation_key, &authority_id, &schema_version);
    if authority_id.trim().is_empty()
        || schema_version != MEMORY_AUTHORITY_SCHEMA_VERSION
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
    let quick_check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(authority_unavailable)?;
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
    let mut events = connection
        .prepare(
            "SELECT persona_id, deletion_id
             FROM deletion_event
             ORDER BY authority_revision",
        )
        .map_err(authority_unavailable)?;
    let identities = events
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(authority_unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(authority_unavailable)?;
    drop(events);
    for (persona_id, deletion_id) in identities {
        load_event_with_connection(derivation_key, connection, &persona_id, &deletion_id)?
            .ok_or_else(|| MemoryError::new(MemoryErrorCode::DeletionAuthorityUnavailable))?;
    }
    let expected_ledger = authority_ledger_commitment(derivation_key, connection)?;
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
) -> Result<[u8; 32], MemoryError> {
    let mut statement = connection
        .prepare(
            "SELECT authority_revision, deletion_id, persona_id, recorded_at, durable_at
             FROM deletion_event
             ORDER BY authority_revision",
        )
        .map_err(authority_unavailable)?;
    let events = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(authority_unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(authority_unavailable)?;
    drop(statement);

    let mut canonical = Vec::new();
    canonical.extend_from_slice(&(events.len() as u64).to_be_bytes());
    for (revision, deletion_id, persona_id, recorded_at, durable_at) in events {
        canonical.extend_from_slice(&revision.to_be_bytes());
        append_length_prefixed(&mut canonical, deletion_id.as_bytes());
        append_length_prefixed(&mut canonical, persona_id.as_bytes());
        append_length_prefixed(&mut canonical, recorded_at.as_bytes());
        append_length_prefixed(&mut canonical, durable_at.as_bytes());

        let mut subjects = connection
            .prepare(
                "SELECT subject_fingerprint
                 FROM deletion_subject
                 WHERE persona_id = ?1 AND deletion_id = ?2
                 ORDER BY subject_fingerprint",
            )
            .map_err(authority_unavailable)?;
        let fingerprints = subjects
            .query_map(params![persona_id, deletion_id], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .map_err(authority_unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(authority_unavailable)?;
        drop(subjects);
        canonical.extend_from_slice(&(fingerprints.len() as u64).to_be_bytes());
        for fingerprint in fingerprints {
            append_length_prefixed(&mut canonical, &fingerprint);
        }
    }
    Ok(keyed_digest_with_key(
        key,
        b"muse-memory-deletion-ledger/v1",
        &[canonical.as_slice()],
    ))
}

fn refresh_ledger_commitment(
    key: &[u8; MEMORY_DERIVATION_KEY_LENGTH],
    connection: &Connection,
) -> Result<(), MemoryError> {
    let commitment = authority_ledger_commitment(key, connection)?;
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
    let result = (|| {
        restrict_sensitive_file_permissions(&temporary).map_err(authority_unavailable)?;
        let connection = Connection::open(&temporary).map_err(authority_unavailable)?;
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
        let ledger_commitment = authority_ledger_commitment(derivation_key, &connection)?;
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
        validate_authority_database(&connection, derivation_key)?;
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
