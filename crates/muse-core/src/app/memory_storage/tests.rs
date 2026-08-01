use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, params};

use super::authority::{fail_next_authority_syncs_for_test, pause_after_authority_commit_for_test};
use super::{SqliteMemoryRepository, normalize_memory_fts_query};
use crate::app::storage::{backup_runtime_database, open_runtime_database};
use crate::domain::memory::{
    ConfirmedMemoryDeleteRequest, MemoryCategory, MemoryCommitEnvelope, MemoryDeleteConfirmation,
    MemoryDeleteConfirmationSource, MemoryDeleteParams, MemoryDeletionAuthority,
    MemoryDeletionAuthorityReceipt, MemoryDeletionAuthorityRequest, MemoryDeletionCheckRequest,
    MemoryDeletionDecision, MemoryDeletionSubject, MemoryError, MemoryErrorCode, MemoryId,
    MemoryImportance, MemoryImportanceAdjustment, MemoryManagementAuthorization,
    MemoryManagementBinding, MemoryManagementContentMutation, MemoryManagementContentParams,
    MemoryMutateParams, MemoryRepository, MemoryRevisionId, MemorySafetyAssessment,
    MemorySafetyFailure, MemorySafetyStage, MemorySensitivityPolicy, MemorySensitivityRequest,
    MemorySourceEligibility, MemoryStagedMutation,
};

const TIME_1: &str = "2026-07-30T01:00:00Z";
const TIME_2: &str = "2026-07-30T02:00:00Z";
const TIME_3: &str = "2026-07-30T03:00:00Z";
const DELETE_EXPIRES_AT: &str = "2099-07-30T03:05:00Z";
static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "muse-memory-{label}-{}-{sequence}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct AllowPolicy;

impl MemorySensitivityPolicy for AllowPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        MemorySafetyAssessment::Allowed {
            stage: request.stage,
            policy_version: "测试策略-v1".to_string(),
        }
    }
}

struct RejectPolicy {
    stage: MemorySafetyStage,
    operation_id: Option<String>,
}

impl MemorySensitivityPolicy for RejectPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        if request.stage == self.stage
            && self
                .operation_id
                .as_deref()
                .is_none_or(|operation_id| operation_id == request.operation_id)
        {
            MemorySafetyAssessment::Rejected {
                stage: request.stage,
                policy_version: Some("测试拒绝策略-v1".to_string()),
            }
        } else {
            AllowPolicy.assess(request)
        }
    }
}

struct FailClosedCommitPolicy;

impl MemorySensitivityPolicy for FailClosedCommitPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        if request.stage == MemorySafetyStage::RepositoryCommit {
            MemorySafetyAssessment::FailClosed {
                stage: request.stage,
                reason: MemorySafetyFailure::Indeterminate,
            }
        } else {
            AllowPolicy.assess(request)
        }
    }
}

struct UnusableAuthority;

impl MemoryDeletionAuthority for UnusableAuthority {
    fn record(
        &self,
        _request: &MemoryDeletionAuthorityRequest,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ))
    }

    fn check(
        &self,
        _request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ))
    }
}

struct AlternateAllowAuthority;

impl MemoryDeletionAuthority for AlternateAllowAuthority {
    fn record(
        &self,
        _request: &MemoryDeletionAuthorityRequest,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ))
    }

    fn check(
        &self,
        _request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        Ok(MemoryDeletionDecision::Allowed)
    }
}

fn scope(persona_id: &str) -> crate::domain::memory::MemoryPersonaScope {
    crate::domain::memory::MemoryPersonaScope::new(persona_id).expect("Persona scope 应有效")
}

/// 精确复刻 2fd7309 写入检索投影时的规范化，供父版本数据库升级夹具使用。
fn normalize_projection_like_2fd7309(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn staged_create(
    scope: &crate::domain::memory::MemoryPersonaScope,
    conversation_id: &str,
    turn_id: &str,
    operation_id: &str,
    memory_id: &str,
    revision_id: &str,
    content: &str,
) -> MemoryStagedMutation {
    let eligibility = MemorySourceEligibility::verify_direct_user_message(
        scope.clone(),
        conversation_id,
        turn_id,
        operation_id,
        "请记住我喜欢这条测试记忆",
        "用户喜欢这条测试记忆",
    )
    .expect("直接用户来源应可验证");
    let binding =
        crate::domain::memory::MemoryRuntimeBinding::new(eligibility, TIME_1, TIME_1, TIME_1)
            .expect("runtime binding 应有效");
    MemoryStagedMutation::stage(
        MemoryMutateParams::Create {
            category: MemoryCategory::UserPreference,
            content: content.to_string(),
            importance: MemoryImportance::Normal,
            event_time: None,
            change_reason: "测试创建".to_string(),
        },
        binding,
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(revision_id.to_string()),
        &AllowPolicy,
    )
    .expect("第一敏感门应允许测试数据")
}

#[allow(clippy::too_many_arguments)]
fn staged_change(
    scope: &crate::domain::memory::MemoryPersonaScope,
    conversation_id: &str,
    turn_id: &str,
    operation_id: &str,
    memory_id: &str,
    expected_revision_id: &str,
    revision_id: &str,
    content: &str,
    correct: bool,
) -> MemoryStagedMutation {
    let eligibility = MemorySourceEligibility::verify_direct_user_message(
        scope.clone(),
        conversation_id,
        turn_id,
        operation_id,
        "请记住我喜欢更新测试记忆",
        "用户喜欢更新测试记忆",
    )
    .expect("直接用户来源应可验证");
    let binding =
        crate::domain::memory::MemoryRuntimeBinding::new(eligibility, TIME_2, TIME_2, TIME_2)
            .expect("runtime binding 应有效");
    let common = (
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(expected_revision_id.to_string()),
        MemoryCategory::UserPreference,
        content.to_string(),
        MemoryImportance::High,
        None,
        "测试变更".to_string(),
    );
    let params = if correct {
        MemoryMutateParams::Correct {
            memory_id: common.0,
            expected_revision_id: common.1,
            category: common.2,
            content: common.3,
            importance: common.4,
            event_time: common.5,
            change_reason: common.6,
        }
    } else {
        MemoryMutateParams::Update {
            memory_id: common.0,
            expected_revision_id: common.1,
            category: common.2,
            content: common.3,
            importance: common.4,
            event_time: common.5,
            change_reason: common.6,
        }
    };
    MemoryStagedMutation::stage(
        params,
        binding,
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(revision_id.to_string()),
        &AllowPolicy,
    )
    .expect("第一敏感门应允许测试数据")
}

fn envelope(
    scope: &crate::domain::memory::MemoryPersonaScope,
    idempotency_key: &str,
    conversation_id: &str,
    turn_id: &str,
    mutations: Vec<MemoryStagedMutation>,
) -> MemoryCommitEnvelope {
    MemoryCommitEnvelope::new(
        idempotency_key,
        scope.clone(),
        conversation_id,
        turn_id,
        TIME_3,
        mutations,
    )
    .expect("commit envelope 应有效")
}

#[allow(clippy::too_many_arguments)]
fn commit_create(
    repository: &SqliteMemoryRepository,
    scope: &crate::domain::memory::MemoryPersonaScope,
    key: &str,
    conversation_id: &str,
    turn_id: &str,
    operation_id: &str,
    memory_id: &str,
    revision_id: &str,
    content: &str,
) -> MemoryCommitEnvelope {
    let envelope = envelope(
        scope,
        key,
        conversation_id,
        turn_id,
        vec![staged_create(
            scope,
            conversation_id,
            turn_id,
            operation_id,
            memory_id,
            revision_id,
            content,
        )],
    );
    repository
        .apply_committed_batch(&envelope, &AllowPolicy)
        .expect("创建应成功");
    envelope
}

fn confirmed_delete(
    scope: &crate::domain::memory::MemoryPersonaScope,
    deletion_id: &str,
    params: MemoryDeleteParams,
) -> ConfirmedMemoryDeleteRequest {
    let confirmation = MemoryDeleteConfirmation::new(
        deletion_id,
        scope,
        &params,
        TIME_3,
        DELETE_EXPIRES_AT,
        MemoryDeleteConfirmationSource::PersonaManagement {
            action_id: deletion_id.to_string(),
        },
    )
    .expect("删除确认应有效");
    ConfirmedMemoryDeleteRequest::bind(params, scope.clone(), confirmation).expect("删除请求应有效")
}

fn management_binding(
    scope: &crate::domain::memory::MemoryPersonaScope,
    operation_id: &str,
) -> MemoryManagementBinding {
    let authorization = MemoryManagementAuthorization::from_runtime(
        scope.clone(),
        format!("action-{operation_id}"),
        TIME_1,
    )
    .expect("管理授权应有效");
    MemoryManagementBinding::bind(authorization, operation_id, TIME_2, TIME_2, TIME_2)
        .expect("管理绑定应有效")
}

fn management_create(
    scope: &crate::domain::memory::MemoryPersonaScope,
    operation_id: &str,
    memory_id: &str,
    revision_id: &str,
    content: &str,
    sensitivity: &dyn MemorySensitivityPolicy,
) -> Result<MemoryManagementContentMutation, MemoryError> {
    MemoryManagementContentMutation::bind(
        MemoryManagementContentParams::Create {
            category: MemoryCategory::UserPreference,
            content: content.to_string(),
            importance: MemoryImportance::Normal,
            event_time: None,
            change_reason: "管理页创建".to_string(),
        },
        management_binding(scope, operation_id),
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(revision_id.to_string()),
        sensitivity,
    )
}

#[allow(clippy::too_many_arguments)]
fn management_correct(
    scope: &crate::domain::memory::MemoryPersonaScope,
    operation_id: &str,
    memory_id: &str,
    expected_revision_id: &str,
    assigned_revision_id: &str,
    content: &str,
    sensitivity: &dyn MemorySensitivityPolicy,
) -> Result<MemoryManagementContentMutation, MemoryError> {
    MemoryManagementContentMutation::bind(
        MemoryManagementContentParams::Correct {
            memory_id: MemoryId(memory_id.to_string()),
            expected_revision_id: MemoryRevisionId(expected_revision_id.to_string()),
            category: MemoryCategory::UserPreference,
            content: content.to_string(),
            event_time: None,
            change_reason: "管理页纠正".to_string(),
        },
        management_binding(scope, operation_id),
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(assigned_revision_id.to_string()),
        sensitivity,
    )
}

fn importance_adjustment(
    scope: &crate::domain::memory::MemoryPersonaScope,
    operation_id: &str,
    memory_id: &str,
    expected_revision_id: &str,
    expected_importance: MemoryImportance,
    importance: MemoryImportance,
) -> MemoryImportanceAdjustment {
    MemoryImportanceAdjustment::bind(
        management_binding(scope, operation_id),
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(expected_revision_id.to_string()),
        expected_importance,
        importance,
    )
    .expect("重要程度调整应有效")
}

fn seed_schema_version_seven(root: &Path) {
    let (_, connection) = open_runtime_database(root).expect("应先生成真实 1-7 schema");
    drop(connection);
    downgrade_runtime_to_v7(root);
    let privacy = root.join("privacy");
    if let Err(error) = fs::remove_dir_all(&privacy)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        panic!("应清理真实 v7 基线附带的 v8 权威：{error}");
    }
}

fn downgrade_runtime_to_v7(root: &Path) {
    let connection = Connection::open(root.join("runtime/muse.sqlite")).expect("应打开 runtime 库");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             BEGIN IMMEDIATE;
             DROP TRIGGER IF EXISTS memory_search_projection_insert;
             DROP TRIGGER IF EXISTS memory_search_projection_delete;
             DROP TRIGGER IF EXISTS memory_search_projection_update;
             DROP TABLE IF EXISTS memory_fts;
             DROP TABLE IF EXISTS memory_search_projection;
             DROP TABLE IF EXISTS memory_management_operation;
             DROP TABLE IF EXISTS memory_committed_operation;
             DROP TABLE IF EXISTS memory_committed_batch;
             DROP TABLE IF EXISTS memory_revision_source;
             DROP TABLE IF EXISTS memory_revision;
             DROP TABLE IF EXISTS memory_entry;
             DROP TABLE IF EXISTS memory_authority_anchor;
             DELETE FROM schema_migrations WHERE version = 8;
             COMMIT;
             PRAGMA foreign_keys = ON;",
        )
        .expect("应只移除 migration 8 对象");
    let checkpoint: (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("应收口 v7 runtime");
    assert_eq!(checkpoint.0, 0, "v7 runtime checkpoint 不得 busy");
}

fn seed_completed_deletion(
    root: &Path,
    label: &str,
) -> (crate::domain::memory::MemoryPersonaScope, MemoryId) {
    let repository = SqliteMemoryRepository::open(root).expect("应初始化 v8 Repository");
    let scope = scope(&format!("persona-v7-{label}"));
    let memory_id = MemoryId(format!("memory-v7-{label}"));
    commit_create(
        &repository,
        &scope,
        &format!("batch-v7-{label}"),
        &format!("conversation-v7-{label}"),
        &format!("turn-v7-{label}"),
        &format!("operation-v7-{label}"),
        &memory_id.0,
        &format!("revision-v7-{label}"),
        "用于证明 v7 回退不能重生权威的事实",
    );
    let delete = confirmed_delete(
        &scope,
        &format!("delete-v7-{label}"),
        MemoryDeleteParams::Memory {
            memory_id: memory_id.clone(),
        },
    );
    repository
        .delete_confirmed(&delete, repository.deletion_authority())
        .expect("应形成正式 tombstone");
    drop(repository);
    (scope, memory_id)
}

#[test]
fn 删除确认只允许同_persona_同目标_同_intent_精确重放() {
    let root = TestDirectory::new("delete-confirmation-binding");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let persona_a = scope("persona-confirm-a");
    let persona_b = scope("persona-confirm-b");
    commit_create(
        &repository,
        &persona_a,
        "batch-confirm-a",
        "conversation-confirm-a",
        "turn-confirm-a",
        "operation-confirm-a",
        "memory-confirm-a",
        "revision-confirm-a",
        "用户喜欢夜间散步",
    );
    commit_create(
        &repository,
        &persona_b,
        "batch-confirm-b",
        "conversation-confirm-b",
        "turn-confirm-b",
        "operation-confirm-b",
        "memory-confirm-b",
        "revision-confirm-b",
        "用户喜欢清晨散步",
    );

    let original = confirmed_delete(
        &persona_a,
        "confirmation-global-1",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("memory-confirm-a".to_string()),
        },
    );
    let first = repository
        .delete_confirmed(&original, repository.deletion_authority())
        .expect("首次确认删除应成功");
    let replay = repository
        .delete_confirmed(&original, repository.deletion_authority())
        .expect("完全相同的确认应幂等重放");
    assert_eq!(first, replay);

    let same_target = MemoryDeleteParams::Memory {
        memory_id: MemoryId("memory-confirm-a".to_string()),
    };
    let changed_source = MemoryDeleteConfirmation::new(
        "confirmation-global-1",
        &persona_a,
        &same_target,
        TIME_3,
        DELETE_EXPIRES_AT,
        MemoryDeleteConfirmationSource::ConversationTurn {
            conversation_id: "conversation-confirm-a".to_string(),
            turn_id: "turn-confirm-a".to_string(),
            approval_id: "confirmation-global-1".to_string(),
            call_id: "call-confirm-conflict".to_string(),
        },
    )
    .expect("换来源的确认本身应可解析");
    let changed_source =
        ConfirmedMemoryDeleteRequest::bind(same_target, persona_a.clone(), changed_source)
            .expect("换来源请求应能进入 Repository 冲突校验");
    assert_eq!(
        repository
            .delete_confirmed(&changed_source, repository.deletion_authority())
            .expect_err("同确认同目标换来源 call 必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );

    let changed_target = confirmed_delete(
        &persona_a,
        "confirmation-global-1",
        MemoryDeleteParams::PersonaAll,
    );
    assert_eq!(
        repository
            .delete_confirmed(&changed_target, repository.deletion_authority())
            .expect_err("同确认换目标必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );

    let cross_persona = confirmed_delete(
        &persona_b,
        "confirmation-global-1",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("memory-confirm-b".to_string()),
        },
    );
    assert_eq!(
        repository
            .delete_confirmed(&cross_persona, repository.deletion_authority())
            .expect_err("跨 Persona 复用 confirmation ID 必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );
    assert!(
        repository
            .current(&persona_b, &MemoryId("memory-confirm-b".to_string()))
            .expect("Persona B 查询应成功")
            .is_some(),
        "跨 Persona 冲突不得误删另一角色记忆"
    );
}

fn seed_used_authority_then_downgrade(
    root: &Path,
    label: &str,
) -> (
    crate::domain::memory::MemoryPersonaScope,
    MemoryId,
    String,
    Vec<u8>,
) {
    let (scope, memory_id) = seed_completed_deletion(root, label);
    let authority_path = root.join("privacy/memory-deletion-authority.sqlite");
    let authority = Connection::open(&authority_path).expect("应打开正式权威库");
    let (authority_id, event_count): (String, i64) = authority
        .query_row(
            "SELECT meta.authority_id, COUNT(event.deletion_id)
             FROM authority_meta AS meta
             JOIN deletion_event AS event
             WHERE meta.singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("应读取正式权威证据");
    assert_eq!(event_count, 1);
    drop(authority);
    let key = fs::read(root.join("privacy/memory-derivation.key")).expect("应读取正式 key");
    downgrade_runtime_to_v7(root);
    (scope, memory_id, authority_id, key)
}

fn file_contains(path: &Path, needle: &[u8]) -> bool {
    fs::read(path)
        .ok()
        .is_some_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
}

fn assert_storage_does_not_contain(root: &Path, backup: &Path, sentinel: &str) {
    for path in [
        root.join("runtime/muse.sqlite"),
        root.join("runtime/muse.sqlite-wal"),
        root.join("runtime/muse.sqlite-shm"),
        root.join("privacy/memory-deletion-authority.sqlite"),
        root.join("privacy/memory-derivation.key"),
        backup.to_path_buf(),
    ] {
        assert!(
            !file_contains(&path, sentinel.as_bytes()),
            "受保护正文不应出现在 `{}`",
            path.display()
        );
    }
}

#[test]
fn migration_八覆盖空库_v7升级与完整性约束() {
    let empty = TestDirectory::new("migration-empty");
    let (database_path, connection) = open_runtime_database(empty.path()).expect("空库应迁移到 v8");
    let latest: (i64, String) = connection
        .query_row(
            "SELECT version, name FROM schema_migrations ORDER BY version DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("应读取 migration");
    assert_eq!(latest, (8, "persona_long_term_memory_storage".to_string()));
    for table in [
        "memory_authority_anchor",
        "memory_entry",
        "memory_revision",
        "memory_revision_source",
        "memory_committed_batch",
        "memory_committed_operation",
        "memory_search_projection",
        "memory_fts",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE name = ?1
                 )",
                [table],
                |row| row.get(0),
            )
            .expect("应核对记忆表");
        assert!(exists, "migration 8 应创建 `{table}`");
    }
    let quick_check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("应运行 quick_check");
    assert_eq!(quick_check, "ok");
    assert!(
        connection
            .prepare("PRAGMA foreign_key_check")
            .expect("应准备外键检查")
            .query([])
            .expect("应运行外键检查")
            .next()
            .expect("应读取外键结果")
            .is_none()
    );
    let anchor_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM memory_authority_anchor", [], |row| {
            row.get(0)
        })
        .expect("应读取 anchor");
    assert_eq!(anchor_count, 1);
    let fts_secure_delete: i64 = connection
        .query_row(
            "SELECT v FROM memory_fts_config WHERE k = 'secure-delete'",
            [],
            |row| row.get(0),
        )
        .expect("FTS5 应持久启用 secure-delete");
    assert_eq!(fts_secure_delete, 1);
    drop(connection);
    assert!(database_path.is_file());

    let upgraded = TestDirectory::new("migration-v7");
    seed_schema_version_seven(upgraded.path());
    let (_, upgraded_connection) =
        open_runtime_database(upgraded.path()).expect("v7 应可首次初始化权威并升级");
    let latest: i64 = upgraded_connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("应读取升级版本");
    assert_eq!(latest, 8);
    let verifier_lengths: (i64, i64) = Connection::open(
        upgraded
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
    )
    .expect("应打开权威库")
    .query_row(
        "SELECT length(key_verifier), length(ledger_commitment)
         FROM authority_meta WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("应读取 key verifier 与 ledger commitment");
    assert_eq!(verifier_lengths, (32, 32));
}

#[test]
fn migration_八缺失_anchor_或权威时拒绝开放() {
    let missing_anchor = TestDirectory::new("missing-anchor");
    fs::create_dir_all(missing_anchor.path().join("runtime")).expect("应创建 runtime");
    let connection =
        Connection::open(missing_anchor.path().join("runtime/muse.sqlite")).expect("应建库");
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations(
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at TEXT NOT NULL
             );
             INSERT INTO schema_migrations
             VALUES(8, 'persona_long_term_memory_storage', '2026-07-30T00:00:00Z');
             CREATE TABLE memory_authority_anchor (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                authority_id TEXT NOT NULL,
                initialized_at TEXT NOT NULL,
                last_applied_revision INTEGER NOT NULL DEFAULT 0
                    CHECK (last_applied_revision >= 0)
             );",
        )
        .expect("应模拟 anchor 行缺失的 v8");
    drop(connection);
    let error = open_runtime_database(missing_anchor.path())
        .expect_err("缺失 anchor 的 v8 必须 fail closed");
    assert!(
        matches!(
            error,
            crate::app::storage::RuntimeStorageError::Integrity(_)
        ),
        "缺失 anchor 必须为 Integrity，实际为：{error}"
    );

    let missing_authority = TestDirectory::new("missing-authority");
    let (_, connection) = open_runtime_database(missing_authority.path()).expect("应初始化完整 v8");
    drop(connection);
    fs::remove_file(
        missing_authority
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
    )
    .expect("应删除测试权威");
    let error = open_runtime_database(missing_authority.path())
        .expect_err("删除权威缺失时必须 fail closed");
    assert!(
        matches!(
            error,
            crate::app::storage::RuntimeStorageError::Integrity(_)
        ),
        "删除权威缺失必须为 Integrity，实际为：{error}"
    );
}

#[test]
fn authority_落后于_runtime_anchor_或数据库损坏时拒绝开放() {
    let authority_behind = TestDirectory::new("authority-behind-anchor");
    drop(SqliteMemoryRepository::open(authority_behind.path()).expect("应初始化 Repository"));
    let connection =
        Connection::open(authority_behind.path().join("runtime/muse.sqlite")).expect("应打开主库");
    connection
        .execute(
            "UPDATE memory_authority_anchor
             SET last_applied_revision = last_applied_revision + 1
             WHERE singleton = 1",
            [],
        )
        .expect("应模拟 runtime anchor 超前");
    drop(connection);
    let error = SqliteMemoryRepository::open(authority_behind.path())
        .expect_err("删除权威落后于 runtime anchor 时必须 fail closed");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);

    let damaged_authority = TestDirectory::new("authority-damaged");
    drop(SqliteMemoryRepository::open(damaged_authority.path()).expect("应初始化 Repository"));
    fs::write(
        damaged_authority
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
        b"damaged-authority",
    )
    .expect("应损坏测试权威库");
    let error = SqliteMemoryRepository::open(damaged_authority.path())
        .expect_err("已有 v8 anchor 时损坏权威不得开放");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);
}

#[test]
fn authority_账本承诺拒绝_subject_篡改缺行_completed_event_缺失与字段篡改() {
    let subject_tampered = TestDirectory::new("authority-subject-tampered");
    seed_completed_deletion(subject_tampered.path(), "subject-tampered");
    let authority_path = subject_tampered
        .path()
        .join("privacy/memory-deletion-authority.sqlite");
    let authority = Connection::open(&authority_path).expect("应打开 subject 篡改库");
    authority
        .execute(
            "UPDATE deletion_subject
             SET memory_id = 'tampered-memory-id'
             WHERE subject_kind = 'memory'",
            [],
        )
        .expect("应篡改 subject 字段");
    drop(authority);
    let error = SqliteMemoryRepository::open(subject_tampered.path())
        .expect_err("subject 字段与 fingerprint 不一致时必须 fail closed");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);

    let subject_missing = TestDirectory::new("authority-subject-missing");
    seed_completed_deletion(subject_missing.path(), "subject-missing");
    let authority = Connection::open(
        subject_missing
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
    )
    .expect("应打开 subject 缺行库");
    assert_eq!(
        authority
            .execute(
                "DELETE FROM deletion_subject
                 WHERE rowid = (
                    SELECT rowid FROM deletion_subject
                    WHERE subject_kind = 'source_turn'
                    LIMIT 1
                 )",
                [],
            )
            .expect("应删除单个 subject 行"),
        1
    );
    drop(authority);
    let error = SqliteMemoryRepository::open(subject_missing.path())
        .expect_err("subject 整行缺失必须被 ledger commitment 发现");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);

    let completed_event_missing = TestDirectory::new("authority-completed-event-missing");
    seed_completed_deletion(completed_event_missing.path(), "event-first");
    seed_completed_deletion(completed_event_missing.path(), "event-second");
    let authority = Connection::open(
        completed_event_missing
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
    )
    .expect("应打开 completed event 缺失库");
    authority
        .pragma_update(None, "foreign_keys", "ON")
        .expect("应启用级联删除");
    assert_eq!(
        authority
            .execute(
                "DELETE FROM deletion_event
                 WHERE deletion_id = 'delete-v7-event-first'",
                [],
            )
            .expect("应删除非最新 completed event"),
        1
    );
    drop(authority);
    let error = SqliteMemoryRepository::open(completed_event_missing.path())
        .expect_err("非最新 completed event 连同 subjects 缺失必须 fail closed");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);

    let completed_event_tampered = TestDirectory::new("authority-completed-event-tampered");
    seed_completed_deletion(completed_event_tampered.path(), "event-tampered");
    let authority = Connection::open(
        completed_event_tampered
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
    )
    .expect("应打开 completed event 篡改库");
    authority
        .execute(
            "UPDATE deletion_event
             SET cleanup_completed_at = ?1
             WHERE deletion_id = 'delete-v7-event-tampered'",
            [TIME_1],
        )
        .expect("应篡改 completed event 字段");
    drop(authority);
    let error = SqliteMemoryRepository::open(completed_event_tampered.path())
        .expect_err("completed event 字段与 event verifier 不一致时必须 fail closed");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);
}

#[test]
fn authority_首次初始化只收敛_key_only_与零长度数据库() {
    let key_only = TestDirectory::new("authority-init-key-only");
    seed_schema_version_seven(key_only.path());
    fs::create_dir_all(key_only.path().join("privacy")).expect("应创建 privacy");
    fs::write(
        key_only.path().join("privacy/memory-derivation.key"),
        [7_u8; 32],
    )
    .expect("应模拟已发布 key");

    let (_, connection) =
        open_runtime_database(key_only.path()).expect("key-only 首次初始化残留应可收敛");
    let anchor: String = connection
        .query_row(
            "SELECT authority_id FROM memory_authority_anchor WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("应读取新 anchor");
    assert!(!anchor.is_empty());
    drop(connection);
    assert_eq!(
        fs::read(key_only.path().join("privacy/memory-derivation.key")).expect("应读取复用 key"),
        vec![7_u8; 32],
        "key-only 收敛必须复用已发布的合法 key"
    );

    let empty_database = TestDirectory::new("authority-init-empty-database");
    seed_schema_version_seven(empty_database.path());
    fs::create_dir_all(empty_database.path().join("privacy")).expect("应创建 privacy");
    fs::write(
        empty_database.path().join("privacy/memory-derivation.key"),
        [9_u8; 32],
    )
    .expect("应模拟已发布 key");
    fs::write(
        empty_database
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
        [],
    )
    .expect("应模拟零长度数据库残留");
    open_runtime_database(empty_database.path()).expect("零长度首次初始化残留应可收敛");
    SqliteMemoryRepository::open(empty_database.path()).expect("收敛后应可重复开放");
}

#[test]
fn v7_runtime_回退复用正式_tombstone_且缺错_key_或非空损坏权威均拒绝() {
    let reusable = TestDirectory::new("v7-reuse-authority");
    let (scope, memory_id, authority_id, key) =
        seed_used_authority_then_downgrade(reusable.path(), "reuse");
    let (_, connection) =
        open_runtime_database(reusable.path()).expect("真实 v7 回退应复用合法正式权威");
    let restored_anchor: (String, i64) = connection
        .query_row(
            "SELECT authority_id, last_applied_revision
             FROM memory_authority_anchor WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("应读取复用后的 anchor");
    assert_eq!(restored_anchor.0, authority_id);
    assert!(restored_anchor.1 >= 1);
    drop(connection);
    assert_eq!(
        fs::read(reusable.path().join("privacy/memory-derivation.key")).expect("应读取复用 key"),
        key
    );
    let repository = SqliteMemoryRepository::open(reusable.path()).expect("复用后应正常开放");
    let check = MemoryDeletionCheckRequest::new(BTreeSet::from([MemoryDeletionSubject::Memory {
        persona_id: scope.persona_id().to_string(),
        memory_id,
    }]))
    .expect("tombstone 检查应有效");
    assert!(matches!(
        repository
            .deletion_authority()
            .check(&check)
            .expect("复用的 tombstone 应可验证"),
        MemoryDeletionDecision::Blocked { .. }
    ));
    drop(repository);

    let missing_key = TestDirectory::new("v7-missing-used-key");
    seed_used_authority_then_downgrade(missing_key.path(), "missing-key");
    fs::remove_file(missing_key.path().join("privacy/memory-derivation.key"))
        .expect("应删除正式 key");
    let error =
        open_runtime_database(missing_key.path()).expect_err("正式非空权威缺 key 时不得重建");
    assert!(
        matches!(
            error,
            crate::app::storage::RuntimeStorageError::Integrity(_)
        ),
        "正式非空权威缺 key 必须为 Integrity，实际为：{error}"
    );

    let wrong_key = TestDirectory::new("v7-wrong-used-key");
    seed_used_authority_then_downgrade(wrong_key.path(), "wrong-key");
    fs::write(
        wrong_key.path().join("privacy/memory-derivation.key"),
        [0xD3_u8; 32],
    )
    .expect("应替换正式 key");
    let error = open_runtime_database(wrong_key.path()).expect_err("正式非空权威错 key 时不得重建");
    assert!(
        matches!(
            error,
            crate::app::storage::RuntimeStorageError::Integrity(_)
        ),
        "正式非空权威错 key 必须为 Integrity，实际为：{error}"
    );

    let damaged_authority = TestDirectory::new("v7-damaged-used-authority");
    seed_used_authority_then_downgrade(damaged_authority.path(), "damaged-authority");
    fs::write(
        damaged_authority
            .path()
            .join("privacy/memory-deletion-authority.sqlite"),
        b"non-empty-damaged-authority",
    )
    .expect("应损坏正式非空权威");
    let error =
        open_runtime_database(damaged_authority.path()).expect_err("正式非空权威损坏时不得重建");
    assert!(
        matches!(
            error,
            crate::app::storage::RuntimeStorageError::Integrity(_)
        ),
        "正式非空权威损坏必须为 Integrity，实际为：{error}"
    );
}

#[test]
fn authority_key_替换后即使长度合法也必须_fail_closed() {
    let root = TestDirectory::new("authority-key-verifier");
    drop(SqliteMemoryRepository::open(root.path()).expect("应初始化 Repository"));
    fs::write(
        root.path().join("privacy/memory-derivation.key"),
        [0xA5_u8; 32],
    )
    .expect("应替换测试 key");

    let error = SqliteMemoryRepository::open(root.path()).expect_err("key verifier 应拒绝替换");
    assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);
}

#[test]
fn create_update_correct_原子切换_current_且_corrected_不进_fts() {
    let root = TestDirectory::new("revision-chain");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-a");
    commit_create(
        &repository,
        &scope,
        "batch-create",
        "conversation-1",
        "turn-1",
        "operation-create",
        "memory-1",
        "revision-1",
        "最初喜欢茉莉花",
    );
    let update = envelope(
        &scope,
        "batch-update",
        "conversation-2",
        "turn-2",
        vec![staged_change(
            &scope,
            "conversation-2",
            "turn-2",
            "operation-update",
            "memory-1",
            "revision-1",
            "revision-2",
            "后来喜欢向日葵",
            false,
        )],
    );
    repository
        .apply_committed_batch(&update, &AllowPolicy)
        .expect("update 应成功");
    let correction = envelope(
        &scope,
        "batch-correct",
        "conversation-3",
        "turn-3",
        vec![staged_change(
            &scope,
            "conversation-3",
            "turn-3",
            "operation-correct",
            "memory-1",
            "revision-2",
            "revision-3",
            "其实喜欢海棠花",
            true,
        )],
    );
    repository
        .apply_committed_batch(&correction, &AllowPolicy)
        .expect("correct 应成功");

    let current = repository
        .current(&scope, &MemoryId("memory-1".to_string()))
        .expect("读取应成功")
        .expect("当前记忆应存在");
    assert_eq!(current.current_revision.revision_id.0, "revision-3");
    assert_eq!(current.current_revision.content, "其实喜欢海棠花");
    assert!(
        repository
            .search_current_fts(&scope, "向日葵", 10)
            .expect("应搜索 corrected 旧正文")
            .is_empty()
    );
    assert_eq!(
        repository
            .search_current_fts(&scope, "海棠花", 10)
            .expect("应搜索 current 正文")
            .len(),
        1
    );

    let (_, connection) = open_runtime_database(root.path()).expect("应检查 revision 链");
    let states = connection
        .prepare(
            "SELECT revision_id, state FROM memory_revision
             WHERE persona_id = ?1 AND memory_id = ?2 ORDER BY row_id",
        )
        .expect("应准备 revision 查询")
        .query_map(params!["persona-a", "memory-1"], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("应查询 revision")
        .collect::<Result<Vec<_>, _>>()
        .expect("应收集 revision");
    assert_eq!(
        states,
        vec![
            ("revision-1".to_string(), "superseded".to_string()),
            ("revision-2".to_string(), "corrected".to_string()),
            ("revision-3".to_string(), "current".to_string()),
        ]
    );
    let projection_revision: String = connection
        .query_row(
            "SELECT revision_id FROM memory_search_projection
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-a", "memory-1"],
            |row| row.get(0),
        )
        .expect("应读取 FTS 投影");
    assert_eq!(projection_revision, "revision-3");
}

#[test]
fn current_与_search_对非_active_current_或已结束_revision_一律_fail_closed() {
    let root = TestDirectory::new("current-state-fail-closed");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-current-state");
    commit_create(
        &repository,
        &scope,
        "state-create",
        "state-conversation",
        "state-turn",
        "state-operation",
        "state-memory",
        "state-revision",
        "逻辑状态损坏时绝不能暴露的正文",
    );

    let assert_hidden = || {
        assert!(
            repository
                .current(&scope, &MemoryId("state-memory".to_string()))
                .expect("逻辑损坏时 current 应安全返回")
                .is_none()
        );
        assert!(
            repository
                .search_current_fts(&scope, "绝不能暴露", 10)
                .expect("逻辑损坏时 search 应安全返回")
                .is_empty()
        );
    };

    let connection = repository.open_connection().expect("应打开状态篡改连接");
    connection
        .execute(
            "UPDATE memory_revision
             SET state = 'corrected'
             WHERE persona_id = ?1 AND memory_id = ?2 AND revision_id = ?3",
            params!["persona-current-state", "state-memory", "state-revision"],
        )
        .expect("应篡改 revision state");
    drop(connection);
    assert_hidden();

    let connection = repository.open_connection().expect("应打开状态恢复连接");
    connection
        .execute(
            "UPDATE memory_revision
             SET state = 'current', valid_to = ?1
             WHERE persona_id = ?2 AND memory_id = ?3 AND revision_id = ?4",
            params![
                TIME_3,
                "persona-current-state",
                "state-memory",
                "state-revision"
            ],
        )
        .expect("应模拟 current revision 已结束");
    drop(connection);
    assert_hidden();

    let connection = repository.open_connection().expect("应打开 entry 篡改连接");
    connection
        .execute(
            "UPDATE memory_revision
             SET valid_to = NULL
             WHERE persona_id = ?1 AND memory_id = ?2 AND revision_id = ?3",
            params!["persona-current-state", "state-memory", "state-revision"],
        )
        .expect("应恢复 revision valid_to");
    connection
        .execute(
            "UPDATE memory_entry
             SET state = 'deleted'
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-current-state", "state-memory"],
        )
        .expect("应篡改 entry state");
    drop(connection);
    assert_hidden();
}

#[test]
fn 并发_revision_冲突只允许一个事务获胜() {
    let root = TestDirectory::new("revision-conflict");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-race");
    commit_create(
        &repository,
        &scope,
        "race-create",
        "conversation-race-0",
        "turn-race-0",
        "operation-race-0",
        "memory-race",
        "revision-race-0",
        "并发前的稳定事实",
    );
    drop(repository);

    let first = SqliteMemoryRepository::open(root.path()).expect("应打开第一个实例");
    let second = SqliteMemoryRepository::open(root.path()).expect("应打开第二个实例");
    let first_envelope = envelope(
        &scope,
        "race-update-1",
        "conversation-race-1",
        "turn-race-1",
        vec![staged_change(
            &scope,
            "conversation-race-1",
            "turn-race-1",
            "operation-race-1",
            "memory-race",
            "revision-race-0",
            "revision-race-1",
            "并发获胜候选甲",
            false,
        )],
    );
    let second_envelope = envelope(
        &scope,
        "race-update-2",
        "conversation-race-2",
        "turn-race-2",
        vec![staged_change(
            &scope,
            "conversation-race-2",
            "turn-race-2",
            "operation-race-2",
            "memory-race",
            "revision-race-0",
            "revision-race-2",
            "并发获胜候选乙",
            false,
        )],
    );
    let barrier = Arc::new(Barrier::new(3));
    let first_barrier = Arc::clone(&barrier);
    let first_handle = thread::spawn(move || {
        first_barrier.wait();
        first.apply_committed_batch(&first_envelope, &AllowPolicy)
    });
    let second_barrier = Arc::clone(&barrier);
    let second_handle = thread::spawn(move || {
        second_barrier.wait();
        second.apply_committed_batch(&second_envelope, &AllowPolicy)
    });
    barrier.wait();
    let results = [
        first_handle.join().expect("第一个线程不应 panic"),
        second_handle.join().expect("第二个线程不应 panic"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .filter(|error| error.code() == MemoryErrorCode::RevisionConflict)
            .count(),
        1
    );
}

#[test]
fn 批量第二门故障全回滚且成功重放严格幂等() {
    let root = TestDirectory::new("batch-rollback");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-batch");
    let batch = envelope(
        &scope,
        "batch-atomic",
        "conversation-batch",
        "turn-batch",
        vec![
            staged_create(
                &scope,
                "conversation-batch",
                "turn-batch",
                "operation-batch-1",
                "memory-batch-1",
                "revision-batch-1",
                "批量候选正文甲",
            ),
            staged_create(
                &scope,
                "conversation-batch",
                "turn-batch",
                "operation-batch-2",
                "memory-batch-2",
                "revision-batch-2",
                "批量候选正文乙",
            ),
        ],
    );
    let rejected_operation_id = batch.mutations()[1].binding().operation_id().to_string();
    let error = repository
        .apply_committed_batch(
            &batch,
            &RejectPolicy {
                stage: MemorySafetyStage::RepositoryCommit,
                operation_id: Some(rejected_operation_id),
            },
        )
        .expect_err("第二门拒绝应回滚全批");
    assert_eq!(error.code(), MemoryErrorCode::SensitiveContentRejected);
    let (_, connection) = open_runtime_database(root.path()).expect("应检查回滚");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM memory_entry WHERE persona_id = ?1",
            ["persona-batch"],
            |row| row.get(0),
        )
        .expect("应读取 entry 数量");
    assert_eq!(count, 0);
    drop(connection);

    let first = repository
        .apply_committed_batch(&batch, &AllowPolicy)
        .expect("故障后重放应成功");
    let replay = repository
        .apply_committed_batch(&batch, &AllowPolicy)
        .expect("同一封套重放应返回原收据");
    assert_eq!(first, replay);
    let (_, connection) = open_runtime_database(root.path()).expect("应检查幂等行数");
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM memory_revision WHERE persona_id = ?1",
            ["persona-batch"],
            |row| row.get(0),
        )
        .expect("应读取 revision 数量");
    assert_eq!(revision_count, 2);
}

#[test]
fn committed_turn_唯一且幂等收据逐_operation_核验() {
    let root = TestDirectory::new("batch-receipt");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-receipt");
    let original = commit_create(
        &repository,
        &scope,
        "receipt-key-1",
        "conversation-receipt",
        "turn-receipt",
        "operation-receipt",
        "memory-receipt",
        "revision-receipt",
        "需要核验的持久事实",
    );
    let original_operation_id = original.mutations()[0].binding().operation_id().to_string();
    let duplicate_turn = envelope(
        &scope,
        "receipt-key-2",
        "conversation-receipt",
        "turn-receipt",
        vec![staged_create(
            &scope,
            "conversation-receipt",
            "turn-receipt",
            "operation-receipt-2",
            "memory-receipt-2",
            "revision-receipt-2",
            "不应重复提交的事实",
        )],
    );
    assert_eq!(
        repository
            .apply_committed_batch(&duplicate_turn, &AllowPolicy)
            .expect_err("同一 committed Turn 更换 key 必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );

    for (label, table, field, tampered, repaired) in [
        (
            "operation_id",
            "memory_committed_operation",
            "operation_id",
            "operation-tampered",
            original_operation_id.as_str(),
        ),
        (
            "operation_ordinal",
            "memory_committed_operation",
            "operation_ordinal",
            "7",
            "0",
        ),
        (
            "change_type",
            "memory_committed_operation",
            "change_type",
            "update",
            "create",
        ),
        (
            "memory_id",
            "memory_committed_operation",
            "memory_id",
            "memory-tampered",
            "memory-receipt",
        ),
        (
            "revision_id",
            "memory_committed_operation",
            "revision_id",
            "revision-tampered",
            "revision-receipt",
        ),
        (
            "durable_at",
            "memory_committed_batch",
            "durable_at",
            "不是合法时间",
            TIME_1,
        ),
    ] {
        let connection = crate::app::storage::open_initialized_runtime_database(root.path())
            .expect("应打开收据篡改连接");
        connection
            .execute(
                &format!(
                    "UPDATE {table}
                     SET {field} = ?1
                     WHERE persona_id = ?2 AND idempotency_key = ?3"
                ),
                params![tampered, "persona-receipt", "receipt-key-1"],
            )
            .unwrap_or_else(|error| panic!("应模拟 `{label}` 收据字段损坏：{error}"));
        drop(connection);
        let error = match repository.apply_committed_batch(&original, &AllowPolicy) {
            Ok(_) => panic!("`{label}` 不一致不得返回 durable 收据"),
            Err(error) => error,
        };
        assert_eq!(error.code(), MemoryErrorCode::RepositoryUnavailable);
        let connection = crate::app::storage::open_initialized_runtime_database(root.path())
            .expect("应打开收据修复连接");
        connection
            .execute(
                &format!(
                    "UPDATE {table}
                     SET {field} = ?1
                     WHERE persona_id = ?2 AND idempotency_key = ?3"
                ),
                params![repaired, "persona-receipt", "receipt-key-1"],
            )
            .unwrap_or_else(|error| panic!("应修复 `{label}` 收据字段：{error}"));
    }

    let connection = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应打开缺行篡改连接");
    connection
        .execute(
            "DELETE FROM memory_committed_operation
             WHERE persona_id = ?1 AND idempotency_key = ?2",
            params!["persona-receipt", "receipt-key-1"],
        )
        .expect("应删除唯一 operation 收据行");
    drop(connection);
    assert_eq!(
        repository
            .apply_committed_batch(&original, &AllowPolicy)
            .expect_err("operation 收据缺行不得返回 durable 收据")
            .code(),
        MemoryErrorCode::RepositoryUnavailable
    );

    let connection = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应打开缺行修复连接");
    connection
        .execute(
            "INSERT INTO memory_committed_operation(
                persona_id, idempotency_key, operation_ordinal, operation_id,
                change_type, memory_id, revision_id
             ) VALUES(?1, ?2, 0, ?3, 'create', ?4, ?5)",
            params![
                "persona-receipt",
                "receipt-key-1",
                original_operation_id.as_str(),
                "memory-receipt",
                "revision-receipt"
            ],
        )
        .expect("应恢复唯一 operation 收据行");
    connection
        .execute(
            "INSERT INTO memory_committed_operation(
                persona_id, idempotency_key, operation_ordinal, operation_id,
                change_type, memory_id, revision_id
             ) VALUES(?1, ?2, 1, ?3, 'create', ?4, ?5)",
            params![
                "persona-receipt",
                "receipt-key-1",
                "operation-receipt-extra",
                "memory-receipt-extra",
                "revision-receipt-extra"
            ],
        )
        .expect("应插入额外 operation 收据行");
    drop(connection);
    assert_eq!(
        repository
            .apply_committed_batch(&original, &AllowPolicy)
            .expect_err("operation 收据多行不得返回 durable 收据")
            .code(),
        MemoryErrorCode::RepositoryUnavailable
    );
    let connection = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应打开多行修复连接");
    connection
        .execute(
            "DELETE FROM memory_committed_operation
             WHERE persona_id = ?1
               AND idempotency_key = ?2
               AND operation_ordinal = 1",
            params!["persona-receipt", "receipt-key-1"],
        )
        .expect("应删除额外 operation 收据行");
    drop(connection);

    repository
        .apply_committed_batch(&original, &AllowPolicy)
        .expect("修复全部收据字段后应可幂等重放");
}

#[test]
fn 多op批次_committed收据中间缺行与ordinal乱序拒绝且修复后幂等重放() {
    let root = TestDirectory::new("multi-op-receipt");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-multi-receipt");
    let original = envelope(
        &scope,
        "multi-receipt-key",
        "multi-receipt-conversation",
        "multi-receipt-turn",
        vec![
            staged_create(
                &scope,
                "multi-receipt-conversation",
                "multi-receipt-turn",
                "multi-operation-0",
                "multi-memory-0",
                "multi-revision-0",
                "多op批次事实甲",
            ),
            staged_create(
                &scope,
                "multi-receipt-conversation",
                "multi-receipt-turn",
                "multi-operation-1",
                "multi-memory-1",
                "multi-revision-1",
                "多op批次事实乙",
            ),
            staged_create(
                &scope,
                "multi-receipt-conversation",
                "multi-receipt-turn",
                "multi-operation-2",
                "multi-memory-2",
                "multi-revision-2",
                "多op批次事实丙",
            ),
        ],
    );
    let receipt = repository
        .apply_committed_batch(&original, &AllowPolicy)
        .expect("多 op 批次首次提交应成功");
    assert_eq!(
        receipt.mutations.len(),
        3,
        "首次提交应逐 operation 返回收据"
    );
    let middle_operation_id = original.mutations()[1].binding().operation_id().to_string();
    let repair_missing_middle = format!(
        "INSERT INTO memory_committed_operation(
            persona_id, idempotency_key, operation_ordinal, operation_id,
            change_type, memory_id, revision_id
         ) VALUES(
            'persona-multi-receipt', 'multi-receipt-key', 1, '{middle_operation_id}',
            'create', 'multi-memory-1', 'multi-revision-1'
         )"
    );
    let swap_ordinals = "UPDATE memory_committed_operation SET operation_ordinal = 100
         WHERE persona_id = 'persona-multi-receipt'
           AND idempotency_key = 'multi-receipt-key' AND operation_ordinal = 0;
         UPDATE memory_committed_operation SET operation_ordinal = 0
         WHERE persona_id = 'persona-multi-receipt'
           AND idempotency_key = 'multi-receipt-key' AND operation_ordinal = 2;
         UPDATE memory_committed_operation SET operation_ordinal = 2
         WHERE persona_id = 'persona-multi-receipt'
           AND idempotency_key = 'multi-receipt-key' AND operation_ordinal = 100";

    for (label, tamper_sql, repair_sql) in [
        (
            "中间 operation 收据行缺失",
            "DELETE FROM memory_committed_operation
             WHERE persona_id = 'persona-multi-receipt'
               AND idempotency_key = 'multi-receipt-key'
               AND operation_ordinal = 1",
            repair_missing_middle.as_str(),
        ),
        ("operation ordinal 乱序", swap_ordinals, swap_ordinals),
    ] {
        let connection = crate::app::storage::open_initialized_runtime_database(root.path())
            .expect("应打开多 op 收据篡改连接");
        connection
            .execute_batch(tamper_sql)
            .unwrap_or_else(|error| panic!("应模拟 `{label}`：{error}"));
        drop(connection);
        let error = match repository.apply_committed_batch(&original, &AllowPolicy) {
            Ok(_) => panic!("`{label}` 不得返回 durable 收据"),
            Err(error) => error,
        };
        assert_eq!(
            error.code(),
            MemoryErrorCode::RepositoryUnavailable,
            "`{label}` 必须报 RepositoryUnavailable"
        );
        let connection = crate::app::storage::open_initialized_runtime_database(root.path())
            .expect("应打开多 op 收据修复连接");
        connection
            .execute_batch(repair_sql)
            .unwrap_or_else(|error| panic!("应修复 `{label}`：{error}"));
        drop(connection);
        let receipt = repository
            .apply_committed_batch(&original, &AllowPolicy)
            .unwrap_or_else(|error| panic!("修复 `{label}` 后应可幂等重放：{error}"));
        assert_eq!(
            receipt.mutations.len(),
            3,
            "修复 `{label}` 后重放应逐 operation 返回 durable 收据"
        );
    }
}

#[test]
fn persona_同文同_id_严格隔离且_trigram_支持中文标点_unicode() {
    let root = TestDirectory::new("persona-fts");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let persona_a = scope("persona-a");
    let persona_b = scope("persona-b");
    for (scope, suffix) in [(&persona_a, "a"), (&persona_b, "b")] {
        commit_create(
            &repository,
            scope,
            &format!("fts-key-{suffix}"),
            &format!("fts-conversation-{suffix}"),
            &format!("fts-turn-{suffix}"),
            &format!("fts-operation-{suffix}"),
            "shared-memory-id",
            &format!("fts-revision-{suffix}"),
            "偏好：Café，喜欢猫咪。",
        );
    }
    assert_eq!(
        repository
            .search_current_fts(&persona_a, "Café!!!", 10)
            .expect("Unicode 查询应成功")
            .len(),
        1
    );
    assert_eq!(
        repository
            .search_current_fts(&persona_b, "喜欢猫", 10)
            .expect("中文 trigram 查询应成功")
            .len(),
        1
    );
    assert_eq!(
        normalize_memory_fts_query("，。!?")
            .expect_err("有效字符不足应拒绝")
            .code(),
        MemoryErrorCode::QueryRejected
    );
    assert!(
        repository
            .current(&persona_a, &MemoryId("shared-memory-id".to_string()))
            .expect("A 读取应成功")
            .is_some()
    );
    assert!(
        repository
            .current(&persona_b, &MemoryId("shared-memory-id".to_string()))
            .expect("B 读取应成功")
            .is_some()
    );
}

#[test]
fn 第一门与第二门拒绝正文不进入任何存储面或_debug() {
    let root = TestDirectory::new("sensitive-gates");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-sensitive");
    let first_sentinel = "拒绝正文FIRST-GATE-847293";
    let first_eligibility = MemorySourceEligibility::verify_direct_user_message(
        scope.clone(),
        "sensitive-conversation-1",
        "sensitive-turn-1",
        "sensitive-operation-1",
        "请记住我喜欢敏感测试记忆",
        "用户喜欢敏感测试记忆",
    )
    .expect("直接用户来源应可验证");
    let first_binding =
        crate::domain::memory::MemoryRuntimeBinding::new(first_eligibility, TIME_1, TIME_1, TIME_1)
            .expect("binding 应有效");
    let first_params = MemoryMutateParams::Create {
        category: MemoryCategory::UserFact,
        content: first_sentinel.to_string(),
        importance: MemoryImportance::Normal,
        event_time: None,
        change_reason: "拒绝原因正文".to_string(),
    };
    assert!(!format!("{first_params:?}").contains(first_sentinel));
    let first_error = MemoryStagedMutation::stage(
        first_params,
        first_binding,
        MemoryId("sensitive-memory-1".to_string()),
        MemoryRevisionId("sensitive-revision-1".to_string()),
        &RejectPolicy {
            stage: MemorySafetyStage::TurnStaging,
            operation_id: None,
        },
    )
    .expect_err("第一门必须拒绝");
    assert!(!format!("{first_error:?}").contains(first_sentinel));

    let second_sentinel = "拒绝正文SECOND-GATE-938475";
    let second = envelope(
        &scope,
        "sensitive-batch-2",
        "sensitive-conversation-2",
        "sensitive-turn-2",
        vec![staged_create(
            &scope,
            "sensitive-conversation-2",
            "sensitive-turn-2",
            "sensitive-operation-2",
            "sensitive-memory-2",
            "sensitive-revision-2",
            second_sentinel,
        )],
    );
    assert!(!format!("{second:?}").contains(second_sentinel));
    let second_error = repository
        .apply_committed_batch(&second, &FailClosedCommitPolicy)
        .expect_err("第二门异常必须 fail closed");
    assert_eq!(second_error.code(), MemoryErrorCode::SensitivityUnavailable);
    assert!(!format!("{second_error:?}").contains(second_sentinel));
    drop(repository);

    let backup = root.path().join("backup.sqlite");
    backup_runtime_database(root.path(), &backup).expect("拒绝后仍应可安全备份");
    assert_storage_does_not_contain(root.path(), &backup, first_sentinel);
    assert_storage_does_not_contain(root.path(), &backup, second_sentinel);
    let backup_connection = Connection::open(&backup).expect("应打开备份");
    let projection_count: i64 = backup_connection
        .query_row("SELECT COUNT(*) FROM memory_search_projection", [], |row| {
            row.get(0)
        })
        .expect("应读取 FTS 投影");
    assert_eq!(projection_count, 0);
}

#[test]
fn repository_最终门在散列与_sql_前拒绝超限写入() {
    let root = TestDirectory::new("repository-field-limits");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-field-limits");

    let mut staged = staged_create(
        &scope,
        "limit-conversation",
        "limit-turn",
        "limit-operation",
        "limit-memory",
        "limit-revision",
        "初始有效正文",
    );
    staged.replace_params_for_repository_test(MemoryMutateParams::Create {
        category: MemoryCategory::UserFact,
        content: "超".repeat(crate::domain::memory::MAX_MEMORY_CONTENT_CHARS + 1),
        importance: MemoryImportance::Normal,
        event_time: None,
        change_reason: "测试 Repository 最终门".to_string(),
    });
    let oversized_envelope = envelope(
        &scope,
        "limit-batch",
        "limit-conversation",
        "limit-turn",
        vec![staged],
    );
    assert_eq!(
        repository
            .apply_committed_batch(&oversized_envelope, &AllowPolicy)
            .expect_err("Repository 必须重验超限正文")
            .code(),
        MemoryErrorCode::InvalidRequest
    );
    assert!(
        repository
            .current(&scope, &MemoryId("limit-memory".to_string()))
            .expect("拒绝后读取应成功")
            .is_none()
    );

    let mut management = management_create(
        &scope,
        "limit-management-operation",
        "limit-management-memory",
        "limit-management-revision",
        "管理初始有效正文",
        &AllowPolicy,
    )
    .expect("管理第一门应允许基准参数");
    management.replace_params_for_repository_test(MemoryManagementContentParams::Create {
        category: MemoryCategory::UserFact,
        content: "管理有效正文".to_string(),
        importance: MemoryImportance::Normal,
        event_time: None,
        change_reason: "因".repeat(crate::domain::memory::MAX_MEMORY_CHANGE_REASON_CHARS + 1),
    });
    assert_eq!(
        repository
            .apply_management_content_mutation(&management, &AllowPolicy)
            .expect_err("管理写入也必须在 Repository 重验变化原因")
            .code(),
        MemoryErrorCode::InvalidRequest
    );
    assert!(
        repository
            .current(&scope, &MemoryId("limit-management-memory".to_string()))
            .expect("管理拒绝后读取应成功")
            .is_none()
    );
}

#[test]
fn management_content_双门幂等冲突与_corrected_隔离() {
    let root = TestDirectory::new("management-content");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-management-content");
    let staging_sentinel = "管理第一门拒绝-SENTINEL-11082";
    let staging_error = management_create(
        &scope,
        "management-staging-reject",
        "management-memory-staging",
        "management-revision-staging",
        staging_sentinel,
        &RejectPolicy {
            stage: MemorySafetyStage::TurnStaging,
            operation_id: None,
        },
    )
    .expect_err("管理 create 第一门必须拒绝");
    assert_eq!(
        staging_error.code(),
        MemoryErrorCode::SensitiveContentRejected
    );

    let create_sentinel = "管理第二门拒绝后再创建-SENTINEL-22091";
    let create = management_create(
        &scope,
        "management-create",
        "management-memory",
        "management-revision-1",
        create_sentinel,
        &AllowPolicy,
    )
    .expect("管理 create 第一门应允许");
    assert!(!format!("{create:?}").contains(create_sentinel));
    let commit_error = repository
        .apply_management_content_mutation(
            &create,
            &RejectPolicy {
                stage: MemorySafetyStage::RepositoryCommit,
                operation_id: None,
            },
        )
        .expect_err("管理 create 第二门必须拒绝");
    assert_eq!(
        commit_error.code(),
        MemoryErrorCode::SensitiveContentRejected
    );
    assert!(
        repository
            .current(&scope, &MemoryId("management-memory".to_string()))
            .expect("拒绝后读取应成功")
            .is_none()
    );
    let absent_backup = root.path().join("management-rejected-backup.sqlite");
    backup_runtime_database(root.path(), &absent_backup).expect("拒绝后应可备份");
    assert_storage_does_not_contain(root.path(), &absent_backup, staging_sentinel);
    assert_storage_does_not_contain(root.path(), &absent_backup, create_sentinel);

    let create_receipt = repository
        .apply_management_content_mutation(&create, &AllowPolicy)
        .expect("管理 create 应成功");
    assert_eq!(
        repository
            .apply_management_content_mutation(&create, &AllowPolicy)
            .expect("相同管理 operation_id 应幂等"),
        create_receipt
    );
    let changed_same_operation = management_create(
        &scope,
        "management-create",
        "management-memory",
        "management-revision-other",
        "同 operation_id 的不同正文",
        &AllowPolicy,
    )
    .expect("不同载荷应可完成第一门");
    assert_eq!(
        repository
            .apply_management_content_mutation(&changed_same_operation, &AllowPolicy)
            .expect_err("同 operation_id 的不同载荷必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );

    let correction_staging_sentinel = "管理纠正第一门拒绝-SENTINEL-33109";
    assert_eq!(
        management_correct(
            &scope,
            "management-correct-staging-reject",
            "management-memory",
            "management-revision-1",
            "management-revision-staging-reject",
            correction_staging_sentinel,
            &RejectPolicy {
                stage: MemorySafetyStage::TurnStaging,
                operation_id: None,
            },
        )
        .expect_err("管理 correct 第一门必须拒绝")
        .code(),
        MemoryErrorCode::SensitiveContentRejected
    );
    let corrected_content = "管理纠正后的唯一当前事实";
    let correction = management_correct(
        &scope,
        "management-correct",
        "management-memory",
        "management-revision-1",
        "management-revision-2",
        corrected_content,
        &AllowPolicy,
    )
    .expect("管理 correct 第一门应允许");
    assert_eq!(
        repository
            .apply_management_content_mutation(&correction, &FailClosedCommitPolicy)
            .expect_err("管理 correct 第二门异常必须 fail closed")
            .code(),
        MemoryErrorCode::SensitivityUnavailable
    );
    assert_eq!(
        repository
            .current(&scope, &MemoryId("management-memory".to_string()))
            .expect("第二门失败后读取应成功")
            .expect("旧 current 应保留")
            .current_revision
            .revision_id,
        MemoryRevisionId("management-revision-1".to_string())
    );
    let correction_receipt = repository
        .apply_management_content_mutation(&correction, &AllowPolicy)
        .expect("管理 correct 应成功");
    assert_eq!(
        repository
            .apply_management_content_mutation(&correction, &AllowPolicy)
            .expect("相同 correct operation_id 应幂等"),
        correction_receipt
    );
    let receipt_connection = repository
        .open_connection()
        .expect("应打开管理收据篡改连接");
    receipt_connection
        .execute(
            "UPDATE memory_management_operation
             SET revision_id = 'management-revision-tampered'
             WHERE persona_id = ?1 AND operation_id = ?2",
            params!["persona-management-content", "management-correct"],
        )
        .expect("应篡改管理收据 revision");
    drop(receipt_connection);
    assert_eq!(
        repository
            .apply_management_content_mutation(&correction, &AllowPolicy)
            .expect_err("管理收据字段与 digest 语义不一致时必须 fail closed")
            .code(),
        MemoryErrorCode::RepositoryUnavailable
    );
    let receipt_connection = repository
        .open_connection()
        .expect("应打开管理收据修复连接");
    receipt_connection
        .execute(
            "UPDATE memory_management_operation
             SET revision_id = 'management-revision-2'
             WHERE persona_id = ?1 AND operation_id = ?2",
            params!["persona-management-content", "management-correct"],
        )
        .expect("应恢复管理收据 revision");
    drop(receipt_connection);
    let stale_correction = management_correct(
        &scope,
        "management-correct-stale",
        "management-memory",
        "management-revision-1",
        "management-revision-3",
        "不得落盘的过期纠正正文",
        &AllowPolicy,
    )
    .expect("过期纠正仍可完成第一门");
    assert_eq!(
        repository
            .apply_management_content_mutation(&stale_correction, &AllowPolicy)
            .expect_err("过期 expected revision 必须冲突")
            .code(),
        MemoryErrorCode::RevisionConflict
    );
    assert!(
        repository
            .search_current_fts(&scope, create_sentinel, 10)
            .expect("旧正文查询应成功")
            .is_empty(),
        "corrected 旧正文不得进入模型读取或 FTS 面"
    );
    assert_eq!(
        repository
            .search_current_fts(&scope, corrected_content, 10)
            .expect("新正文查询应成功")
            .len(),
        1
    );
    let state_connection = repository
        .open_connection()
        .expect("应打开 revision 状态检查连接");
    let mut state_statement = state_connection
        .prepare(
            "SELECT revision_id, state
             FROM memory_revision
             WHERE persona_id = ?1 AND memory_id = ?2
             ORDER BY revision_id",
        )
        .expect("应准备 revision 状态查询");
    let states = state_statement
        .query_map(
            params!["persona-management-content", "management-memory"],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("应查询 revision 状态")
        .collect::<Result<Vec<_>, _>>()
        .expect("应读取 revision 状态");
    drop(state_statement);
    drop(state_connection);
    assert_eq!(
        states,
        vec![
            ("management-revision-1".to_string(), "corrected".to_string()),
            ("management-revision-2".to_string(), "current".to_string()),
        ]
    );

    let delete = confirmed_delete(
        &scope,
        "management-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("management-memory".to_string()),
        },
    );
    repository
        .delete_confirmed(&delete, repository.deletion_authority())
        .expect("管理创建的记忆应可删除");
    assert_eq!(
        repository
            .apply_management_content_mutation(&correction, &AllowPolicy)
            .expect("删除后原 operation_id 仅返回无正文收据"),
        correction_receipt
    );
    let blocked_recreate = management_create(
        &scope,
        "management-recreate",
        "management-memory-new",
        "management-revision-new",
        corrected_content,
        &AllowPolicy,
    )
    .expect("重建候选应通过第一门");
    assert_eq!(
        repository
            .apply_management_content_mutation(&blocked_recreate, &AllowPolicy)
            .expect_err("删除后的相同派生正文不得从管理路径复活")
            .code(),
        MemoryErrorCode::SourceIneligible
    );
    assert!(
        repository
            .current(&scope, &MemoryId("management-memory".to_string()))
            .expect("删除后读取应成功")
            .is_none()
    );
}

#[test]
fn importance_仅更新_entry_并发冲突幂等且删除后不复活() {
    let root = TestDirectory::new("management-importance");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-management-importance");
    commit_create(
        &repository,
        &scope,
        "importance-create",
        "importance-conversation",
        "importance-turn",
        "importance-create-operation",
        "importance-memory",
        "importance-revision",
        "重要程度调整不应产生新 revision",
    );
    drop(repository);

    let high_repository = SqliteMemoryRepository::open(root.path()).expect("应打开 high 实例");
    let low_repository = SqliteMemoryRepository::open(root.path()).expect("应打开 low 实例");
    let high = importance_adjustment(
        &scope,
        "importance-high",
        "importance-memory",
        "importance-revision",
        MemoryImportance::Normal,
        MemoryImportance::High,
    );
    let low = importance_adjustment(
        &scope,
        "importance-low",
        "importance-memory",
        "importance-revision",
        MemoryImportance::Normal,
        MemoryImportance::Low,
    );
    let barrier = Arc::new(Barrier::new(3));
    let high_barrier = Arc::clone(&barrier);
    let high_handle = thread::spawn(move || {
        high_barrier.wait();
        high_repository.adjust_importance(&high)
    });
    let low_barrier = Arc::clone(&barrier);
    let low_handle = thread::spawn(move || {
        low_barrier.wait();
        low_repository.adjust_importance(&low)
    });
    barrier.wait();
    let high_result = high_handle.join().expect("high 线程不应 panic");
    let low_result = low_handle.join().expect("low 线程不应 panic");
    assert_ne!(
        high_result.is_ok(),
        low_result.is_ok(),
        "相同 expected importance 的并发调整必须只成功一个"
    );
    let (winning_operation, winning_importance, winning_receipt, losing_error) =
        match (high_result, low_result) {
            (Ok(receipt), Err(error)) => {
                ("importance-high", MemoryImportance::High, receipt, error)
            }
            (Err(error), Ok(receipt)) => ("importance-low", MemoryImportance::Low, receipt, error),
            _ => unreachable!("前置断言已确保只有一个调整成功"),
        };
    assert_eq!(losing_error.code(), MemoryErrorCode::InvalidStateTransition);

    let repository = SqliteMemoryRepository::open(root.path()).expect("应重开 Repository");
    let winning_replay = importance_adjustment(
        &scope,
        winning_operation,
        "importance-memory",
        "importance-revision",
        MemoryImportance::Normal,
        winning_importance,
    );
    assert_eq!(
        repository
            .adjust_importance(&winning_replay)
            .expect("完全相同的重要程度调整应幂等"),
        winning_receipt
    );
    let alternate_importance = if winning_importance == MemoryImportance::High {
        MemoryImportance::Low
    } else {
        MemoryImportance::High
    };
    let changed_same_operation = importance_adjustment(
        &scope,
        winning_operation,
        "importance-memory",
        "importance-revision",
        MemoryImportance::Normal,
        alternate_importance,
    );
    assert_eq!(
        repository
            .adjust_importance(&changed_same_operation)
            .expect_err("同 operation_id 的不同 importance 语义必须拒绝")
            .code(),
        MemoryErrorCode::InvalidRequest
    );

    let connection = repository
        .open_connection()
        .expect("应打开 importance 检查连接");
    let revision_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM memory_revision
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-management-importance", "importance-memory"],
            |row| row.get(0),
        )
        .expect("应统计 revision");
    assert_eq!(revision_count, 1, "importance 只允许修改 entry");
    drop(connection);
    let current = repository
        .current(&scope, &MemoryId("importance-memory".to_string()))
        .expect("应读取当前记忆")
        .expect("当前记忆应存在");
    assert_eq!(current.entry.importance, winning_importance);
    assert_eq!(
        current.current_revision.revision_id,
        MemoryRevisionId("importance-revision".to_string())
    );

    let wrong_revision = importance_adjustment(
        &scope,
        "importance-wrong-revision",
        "importance-memory",
        "importance-revision-missing",
        winning_importance,
        alternate_importance,
    );
    assert_eq!(
        repository
            .adjust_importance(&wrong_revision)
            .expect_err("错误 expected revision 必须冲突")
            .code(),
        MemoryErrorCode::RevisionConflict
    );
    let wrong_expected_importance = importance_adjustment(
        &scope,
        "importance-wrong-expected",
        "importance-memory",
        "importance-revision",
        alternate_importance,
        MemoryImportance::Normal,
    );
    assert_eq!(
        repository
            .adjust_importance(&wrong_expected_importance)
            .expect_err("错误 expected importance 必须冲突")
            .code(),
        MemoryErrorCode::InvalidStateTransition
    );

    let tamper = repository
        .open_connection()
        .expect("应打开收据篡改检查连接");
    tamper
        .execute(
            "UPDATE memory_management_operation
             SET importance = ?1
             WHERE persona_id = ?2 AND operation_id = ?3",
            params![
                if winning_importance == MemoryImportance::High {
                    "low"
                } else {
                    "high"
                },
                "persona-management-importance",
                winning_operation
            ],
        )
        .expect("应篡改测试收据");
    drop(tamper);
    assert_eq!(
        repository
            .adjust_importance(&winning_replay)
            .expect_err("收据字段与 digest 语义不一致时必须 fail closed")
            .code(),
        MemoryErrorCode::RepositoryUnavailable
    );
    let repair = repository.open_connection().expect("应打开收据修复连接");
    repair
        .execute(
            "UPDATE memory_management_operation
             SET importance = ?1
             WHERE persona_id = ?2 AND operation_id = ?3",
            params![
                if winning_importance == MemoryImportance::High {
                    "high"
                } else {
                    "low"
                },
                "persona-management-importance",
                winning_operation
            ],
        )
        .expect("应恢复测试收据");
    drop(repair);

    let delete = confirmed_delete(
        &scope,
        "importance-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("importance-memory".to_string()),
        },
    );
    repository
        .delete_confirmed(&delete, repository.deletion_authority())
        .expect("应删除重要程度测试记忆");
    assert_eq!(
        repository
            .adjust_importance(&winning_replay)
            .expect("删除后原 importance operation 仅返回无正文收据"),
        winning_receipt
    );
    let after_delete = importance_adjustment(
        &scope,
        "importance-after-delete",
        "importance-memory",
        "importance-revision",
        winning_importance,
        alternate_importance,
    );
    assert_eq!(
        repository
            .adjust_importance(&after_delete)
            .expect_err("删除后新 importance 操作不得旁路复活")
            .code(),
        MemoryErrorCode::MemoryNotFound
    );
    assert!(
        repository
            .current(&scope, &MemoryId("importance-memory".to_string()))
            .expect("删除后读取应成功")
            .is_none()
    );
}

#[test]
fn authority_失败不得清主库且_existing_event_重试重新_fsync() {
    let root = TestDirectory::new("authority-retry");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-authority");
    commit_create(
        &repository,
        &scope,
        "authority-create",
        "authority-conversation",
        "authority-turn",
        "authority-operation",
        "authority-memory",
        "authority-revision",
        "权威失败时必须保留的正文",
    );
    let request = confirmed_delete(
        &scope,
        "authority-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("authority-memory".to_string()),
        },
    );
    assert_eq!(
        repository
            .delete_confirmed(&request, &UnusableAuthority)
            .expect_err("非 canonical authority 必须拒绝")
            .code(),
        MemoryErrorCode::DeletionAuthorityUnavailable
    );
    assert_eq!(
        repository
            .delete_confirmed(&request, &AlternateAllowAuthority)
            .expect_err("返回 Allowed 的替代 authority 也必须拒绝")
            .code(),
        MemoryErrorCode::DeletionAuthorityUnavailable
    );
    assert!(
        repository
            .current(&scope, &MemoryId("authority-memory".to_string()))
            .expect("失败后读取应成功")
            .is_some()
    );

    fail_next_authority_syncs_for_test(
        root.path().join("privacy/memory-deletion-authority.sqlite"),
        2,
    );
    assert_eq!(
        repository
            .delete_confirmed(&request, repository.deletion_authority())
            .expect_err("首次 fsync 故障不得签收")
            .code(),
        MemoryErrorCode::DeletionAuthorityUnavailable
    );
    let connection = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应检查 fsync 故障后的主库");
    let main_row_exists: bool = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM memory_entry
                WHERE persona_id = ?1 AND memory_id = ?2
             )",
            params!["persona-authority", "authority-memory"],
            |row| row.get(0),
        )
        .expect("应读取主库 entry");
    assert!(main_row_exists, "权威 fsync 报错前不得清理主库");
    drop(connection);
    assert!(
        repository
            .current(&scope, &MemoryId("authority-memory".to_string()))
            .expect("权威不确定时读取应 fail closed")
            .is_none()
    );
    assert_eq!(
        repository
            .delete_confirmed(&request, repository.deletion_authority())
            .expect_err("existing event 的第二次 fsync 故障仍不得签收")
            .code(),
        MemoryErrorCode::DeletionAuthorityUnavailable
    );
    repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect("同一 event 第三次重试必须再次 fsync 并完成");
    assert!(
        repository
            .current(&scope, &MemoryId("authority-memory".to_string()))
            .expect("删除后读取应成功")
            .is_none()
    );
}

#[test]
fn 单删不连带同_turn_其他记忆且旧库恢复仍保留() {
    let root = TestDirectory::new("source-turn-isolation");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-source");
    let deleted_sentinel = "应被删除的独立事实-SENTINEL-73419";
    let batch = envelope(
        &scope,
        "source-batch",
        "source-conversation",
        "source-turn",
        vec![
            staged_create(
                &scope,
                "source-conversation",
                "source-turn",
                "source-operation-1",
                "source-memory-1",
                "source-revision-1",
                deleted_sentinel,
            ),
            staged_create(
                &scope,
                "source-conversation",
                "source-turn",
                "source-operation-2",
                "source-memory-2",
                "source-revision-2",
                "必须保留的另一事实",
            ),
        ],
    );
    repository
        .apply_committed_batch(&batch, &AllowPolicy)
        .expect("同 Turn 两条记忆应原子创建");
    let backup = root.path().join("before-delete.sqlite");
    backup_runtime_database(root.path(), &backup).expect("应创建删除前备份");
    assert!(
        file_contains(&backup, deleted_sentinel.as_bytes()),
        "删除前旧备份应真实包含待删正文"
    );

    let request = confirmed_delete(
        &scope,
        "source-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("source-memory-1".to_string()),
        },
    );
    repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect("单删应成功");
    assert!(
        repository
            .current(&scope, &MemoryId("source-memory-1".to_string()))
            .expect("读取被删记忆应成功")
            .is_none()
    );
    assert!(
        repository
            .current(&scope, &MemoryId("source-memory-2".to_string()))
            .expect("读取保留记忆应成功")
            .is_some()
    );
    let source_check =
        MemoryDeletionCheckRequest::new(BTreeSet::from([MemoryDeletionSubject::SourceTurn {
            persona_id: scope.persona_id().to_string(),
            conversation_id: "source-conversation".to_string(),
            turn_id: "source-turn".to_string(),
        }]))
        .expect("SourceTurn 检查应有效");
    assert!(matches!(
        repository
            .deletion_authority()
            .check(&source_check)
            .expect("SourceTurn tombstone 应可读"),
        MemoryDeletionDecision::Blocked { .. }
    ));
    drop(repository);

    for sidecar in [
        root.path().join("runtime/muse.sqlite-wal"),
        root.path().join("runtime/muse.sqlite-shm"),
    ] {
        if let Err(error) = fs::remove_file(&sidecar)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("应清理旧库恢复前的 SQLite sidecar：{error}");
        }
    }
    fs::copy(&backup, root.path().join("runtime/muse.sqlite")).expect("应在同一安装恢复旧主库");
    let restored = SqliteMemoryRepository::open(root.path()).expect("恢复开放前应重放当前删除权威");
    assert!(
        restored
            .current(&scope, &MemoryId("source-memory-1".to_string()))
            .expect("恢复后读取被删记忆应成功")
            .is_none()
    );
    assert!(
        restored
            .current(&scope, &MemoryId("source-memory-2".to_string()))
            .expect("恢复后读取保留记忆应成功")
            .is_some()
    );
    let restored_connection = restored
        .open_connection()
        .expect("应打开恢复后的投影检查连接");
    let projection_contains_deleted: bool = restored_connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM memory_search_projection
                WHERE persona_id = ?1 AND content = ?2
             )",
            params![
                "persona-source",
                normalize_memory_fts_query(deleted_sentinel).expect("sentinel 应可规范化")
            ],
            |row| row.get(0),
        )
        .expect("应检查恢复后的 FTS 投影");
    assert!(
        !projection_contains_deleted,
        "恢复收口后 FTS 投影不得含被删正文"
    );
    let applied_revision: i64 = restored_connection
        .query_row(
            "SELECT last_applied_revision
             FROM memory_authority_anchor WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("应读取恢复后的 anchor revision");
    drop(restored_connection);
    let authority = Connection::open(root.path().join("privacy/memory-deletion-authority.sqlite"))
        .expect("应打开恢复后的权威库");
    let authority_revision: i64 = authority
        .query_row(
            "SELECT COALESCE(MAX(authority_revision), 0) FROM deletion_event",
            [],
            |row| row.get(0),
        )
        .expect("应读取权威最大 revision");
    drop(authority);
    assert_eq!(
        applied_revision, authority_revision,
        "旧库恢复必须把 runtime anchor 推进到当前权威"
    );
    drop(restored);
    let normalized_sentinel =
        normalize_memory_fts_query(deleted_sentinel).expect("sentinel 应可规范化");
    for path in [
        root.path().join("runtime/muse.sqlite"),
        root.path().join("runtime/muse.sqlite-wal"),
        root.path().join("runtime/muse.sqlite-shm"),
    ] {
        assert!(
            !file_contains(&path, deleted_sentinel.as_bytes()),
            "恢复收口后的 `{}` 不得物理残留被删正文",
            path.display()
        );
        assert!(
            !file_contains(&path, normalized_sentinel.as_bytes()),
            "恢复收口后的 `{}` 不得物理残留 FTS 规范化正文",
            path.display()
        );
    }

    let reconciled_backup = root.path().join("after-reconcile-backup.sqlite");
    backup_runtime_database(root.path(), &reconciled_backup).expect("恢复收口后应可再次备份");
    assert!(!file_contains(
        &reconciled_backup,
        deleted_sentinel.as_bytes()
    ));
    assert!(!file_contains(
        &reconciled_backup,
        normalized_sentinel.as_bytes()
    ));
    let backup_connection = Connection::open(&reconciled_backup).expect("应打开恢复后备份");
    let backup_projection_count: i64 = backup_connection
        .query_row(
            "SELECT COUNT(*) FROM memory_search_projection
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-source", "source-memory-1"],
            |row| row.get(0),
        )
        .expect("应检查恢复后备份投影");
    assert_eq!(backup_projection_count, 0);
    drop(backup_connection);

    let second_restart =
        SqliteMemoryRepository::open(root.path()).expect("恢复收口后的二次重启应成功");
    assert!(
        second_restart
            .current(&scope, &MemoryId("source-memory-1".to_string()))
            .expect("二次重启后读取被删记忆应成功")
            .is_none()
    );
    assert!(
        second_restart
            .current(&scope, &MemoryId("source-memory-2".to_string()))
            .expect("二次重启后读取保留记忆应成功")
            .is_some()
    );
}

#[test]
fn persona_all_只清当前具体_subject_未来新记忆仍允许() {
    let root = TestDirectory::new("persona-all");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-clear");
    let first_sentinel = "需要清除的事实甲-PERSONA-ALL-4107";
    let updated_sentinel = "需要清除的事实甲更新-PERSONA-ALL-5208";
    commit_create(
        &repository,
        &scope,
        "clear-create-1",
        "clear-conversation-1",
        "clear-turn-1",
        "clear-operation-1",
        "clear-memory-1",
        "clear-revision-1",
        first_sentinel,
    );
    commit_create(
        &repository,
        &scope,
        "clear-create-2",
        "clear-conversation-2",
        "clear-turn-2",
        "clear-operation-2",
        "clear-memory-2",
        "clear-revision-2",
        "需要清除的事实乙",
    );
    let update = envelope(
        &scope,
        "clear-update-1",
        "clear-conversation-update",
        "clear-turn-update",
        vec![staged_change(
            &scope,
            "clear-conversation-update",
            "clear-turn-update",
            "clear-operation-update",
            "clear-memory-1",
            "clear-revision-1",
            "clear-revision-1-updated",
            updated_sentinel,
            false,
        )],
    );
    repository
        .apply_committed_batch(&update, &AllowPolicy)
        .expect("PersonaAll 前应形成多 revision 记忆");
    let request = confirmed_delete(&scope, "clear-all", MemoryDeleteParams::PersonaAll);
    let receipt = repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect("PersonaAll 应成功");
    assert_eq!(receipt.deleted_memory_count, 2);
    let authority = Connection::open(root.path().join("privacy/memory-deletion-authority.sqlite"))
        .expect("应打开删除权威");
    let subject_counts: (i64, i64, i64, i64) = authority
        .query_row(
            "SELECT
                SUM(subject_kind = 'persona'),
                SUM(subject_kind = 'memory'),
                SUM(subject_kind = 'source_turn'),
                SUM(subject_kind = 'derivation')
             FROM deletion_subject
             WHERE persona_id = ?1",
            ["persona-clear"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("应检查 PersonaAll 具体 subjects");
    assert_eq!(subject_counts.0, 0, "PersonaAll 不得写 Persona tombstone");
    assert_eq!(subject_counts.1, 2, "应枚举两条具体 Memory");
    assert_eq!(subject_counts.2, 3, "应枚举全部 revision 来源 Turn");
    assert_eq!(
        subject_counts.3, 4,
        "应枚举全部 revision 派生摘要并绑定删除确认 intent"
    );
    drop(authority);

    let main = repository
        .open_connection()
        .expect("应打开 PersonaAll 主库检查连接");
    for table in [
        "memory_entry",
        "memory_revision",
        "memory_revision_source",
        "memory_search_projection",
    ] {
        let count: i64 = main
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE persona_id = ?1"),
                ["persona-clear"],
                |row| row.get(0),
            )
            .expect("应检查 PersonaAll 主库清理");
        assert_eq!(count, 0, "PersonaAll 后 `{table}` 必须清空");
    }
    drop(main);
    drop(repository);
    for sentinel in [first_sentinel, updated_sentinel] {
        let normalized = normalize_memory_fts_query(sentinel).expect("sentinel 应可规范化");
        for path in [
            root.path().join("runtime/muse.sqlite"),
            root.path().join("runtime/muse.sqlite-wal"),
            root.path().join("runtime/muse.sqlite-shm"),
        ] {
            assert!(!file_contains(&path, sentinel.as_bytes()));
            assert!(!file_contains(&path, normalized.as_bytes()));
        }
    }

    let repository = SqliteMemoryRepository::open(root.path()).expect("PersonaAll 后重启应成功");
    let blocked_recreate = envelope(
        &scope,
        "clear-recreate-old",
        "clear-conversation-recreate",
        "clear-turn-recreate",
        vec![staged_create(
            &scope,
            "clear-conversation-recreate",
            "clear-turn-recreate",
            "clear-operation-recreate",
            "clear-memory-recreate",
            "clear-revision-recreate",
            updated_sentinel,
        )],
    );
    assert_eq!(
        repository
            .apply_committed_batch(&blocked_recreate, &AllowPolicy)
            .expect_err("PersonaAll 后旧派生正文不得复活")
            .code(),
        MemoryErrorCode::SourceIneligible
    );
    commit_create(
        &repository,
        &scope,
        "clear-future",
        "clear-conversation-future",
        "clear-turn-future",
        "clear-operation-future",
        "clear-memory-future",
        "clear-revision-future",
        "清空后允许形成的新事实",
    );
    assert!(
        repository
            .current(&scope, &MemoryId("clear-memory-future".to_string()))
            .expect("未来记忆读取应成功")
            .is_some()
    );
    assert_eq!(
        repository
            .delete_confirmed(&request, repository.deletion_authority())
            .expect("PersonaAll 原 deletion_id 应严格幂等且不扩大范围"),
        receipt
    );
    assert!(
        repository
            .current(&scope, &MemoryId("clear-memory-future".to_string()))
            .expect("PersonaAll 重放后未来记忆读取应成功")
            .is_some(),
        "PersonaAll 幂等重放不得删除首次 intent 后形成的新记忆"
    );
}

#[test]
fn 删除后同派生正文阻断且无正文收据仍可幂等重放() {
    let root = TestDirectory::new("derivation-block");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-derivation");
    let original = commit_create(
        &repository,
        &scope,
        "derivation-create",
        "derivation-conversation-1",
        "derivation-turn-1",
        "derivation-operation-1",
        "derivation-memory-1",
        "derivation-revision-1",
        "不可重新派生的相同事实",
    );
    let request = confirmed_delete(
        &scope,
        "derivation-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("derivation-memory-1".to_string()),
        },
    );
    repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect("删除应成功");
    repository
        .apply_committed_batch(&original, &AllowPolicy)
        .expect("删除 revision 后原 committed 收据仍应幂等返回");

    let duplicate = envelope(
        &scope,
        "derivation-create-2",
        "derivation-conversation-2",
        "derivation-turn-2",
        vec![staged_create(
            &scope,
            "derivation-conversation-2",
            "derivation-turn-2",
            "derivation-operation-2",
            "derivation-memory-2",
            "derivation-revision-2",
            "不可重新派生的相同事实",
        )],
    );
    assert_eq!(
        repository
            .apply_committed_batch(&duplicate, &AllowPolicy)
            .expect_err("相同派生正文必须被 authority 阻断")
            .code(),
        MemoryErrorCode::SourceIneligible
    );
}

#[test]
fn 单删按相同派生彻底遗忘并报告实际命中数且隔离其他_persona() {
    let root = TestDirectory::new("derivation-delete-count");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope_a = scope("persona-derivation-count-a");
    let scope_b = scope("persona-derivation-count-b");
    let shared_content = "同 Persona 的相同派生必须一起彻底遗忘";
    commit_create(
        &repository,
        &scope_a,
        "derivation-count-create-a1",
        "derivation-count-conversation-a1",
        "derivation-count-turn-a1",
        "derivation-count-operation-a1",
        "derivation-count-memory-a1",
        "derivation-count-revision-a1",
        shared_content,
    );
    commit_create(
        &repository,
        &scope_a,
        "derivation-count-create-a2",
        "derivation-count-conversation-a2",
        "derivation-count-turn-a2",
        "derivation-count-operation-a2",
        "derivation-count-memory-a2",
        "derivation-count-revision-a2",
        shared_content,
    );
    commit_create(
        &repository,
        &scope_b,
        "derivation-count-create-b",
        "derivation-count-conversation-b",
        "derivation-count-turn-b",
        "derivation-count-operation-b",
        "derivation-count-memory-b",
        "derivation-count-revision-b",
        shared_content,
    );

    let request = confirmed_delete(
        &scope_a,
        "derivation-count-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("derivation-count-memory-a1".to_string()),
        },
    );
    let receipt = repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect("相同派生的彻底遗忘应成功");
    assert_eq!(
        receipt.deleted_memory_count, 2,
        "收据必须报告 canonical subjects 实际命中的唯一记忆数"
    );
    for memory_id in ["derivation-count-memory-a1", "derivation-count-memory-a2"] {
        assert!(
            repository
                .current(&scope_a, &MemoryId(memory_id.to_string()))
                .expect("同 Persona 读取应成功")
                .is_none(),
            "同 Persona 的相同派生记忆必须一并清除"
        );
    }
    assert!(
        repository
            .current(&scope_b, &MemoryId("derivation-count-memory-b".to_string()))
            .expect("其他 Persona 读取应成功")
            .is_some(),
        "其他 Persona 的同文记忆不得被连带清除"
    );
}

#[test]
fn 同派生删除构造历史_subject_不动点闭包且旧备份不复活() {
    let root = TestDirectory::new("derivation-history-closure");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope_a = scope("persona-closure-a");
    let scope_b = scope("persona-closure-b");
    let content_x = "闭包派生事实-X";
    let content_y = "闭包派生事实-Y";
    commit_create(
        &repository,
        &scope_a,
        "closure-create-a",
        "closure-conversation-a",
        "closure-turn-a",
        "closure-operation-a",
        "closure-memory-a",
        "closure-revision-a",
        content_x,
    );
    commit_create(
        &repository,
        &scope_a,
        "closure-create-b",
        "closure-conversation-b0",
        "closure-turn-b0",
        "closure-operation-b0",
        "closure-memory-b",
        "closure-revision-b0",
        content_y,
    );
    commit_create(
        &repository,
        &scope_a,
        "closure-create-c",
        "closure-conversation-c",
        "closure-turn-c",
        "closure-operation-c",
        "closure-memory-c",
        "closure-revision-c",
        content_y,
    );
    commit_create(
        &repository,
        &scope_b,
        "closure-create-other",
        "closure-conversation-other",
        "closure-turn-other",
        "closure-operation-other",
        "closure-memory-a",
        "closure-revision-other",
        content_x,
    );
    let old_backup = root.path().join("derivation-history-old.sqlite");
    backup_runtime_database(root.path(), &old_backup).expect("应备份 B 尚为 Y 的旧主库");

    let update_b = envelope(
        &scope_a,
        "closure-update-b",
        "closure-conversation-b1",
        "closure-turn-b1",
        vec![staged_change(
            &scope_a,
            "closure-conversation-b1",
            "closure-turn-b1",
            "closure-operation-b1",
            "closure-memory-b",
            "closure-revision-b0",
            "closure-revision-b1",
            content_x,
            false,
        )],
    );
    repository
        .apply_committed_batch(&update_b, &AllowPolicy)
        .expect("B 从 Y 更新到 X 应成功");
    let delete = confirmed_delete(
        &scope_a,
        "closure-delete-a",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("closure-memory-a".to_string()),
        },
    );
    let receipt = repository
        .delete_confirmed(&delete, repository.deletion_authority())
        .expect("同派生历史闭包删除应成功");
    assert_eq!(
        receipt.deleted_memory_count, 3,
        "A=X 应扩张到 B 的 X，再经 B 的历史 Y 扩张到 C"
    );
    for memory_id in ["closure-memory-a", "closure-memory-b", "closure-memory-c"] {
        assert!(
            repository
                .current(&scope_a, &MemoryId(memory_id.to_string()))
                .expect("闭包删除后读取应成功")
                .is_none()
        );
    }
    assert!(
        repository
            .current(&scope_b, &MemoryId("closure-memory-a".to_string()))
            .expect("其他 Persona 读取应成功")
            .is_some()
    );
    drop(repository);

    for sidecar in [
        root.path().join("runtime/muse.sqlite-wal"),
        root.path().join("runtime/muse.sqlite-shm"),
    ] {
        if let Err(error) = fs::remove_file(&sidecar)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("应清理历史闭包恢复前的 SQLite sidecar：{error}");
        }
    }
    fs::copy(&old_backup, root.path().join("runtime/muse.sqlite"))
        .expect("应在同一安装恢复 B 尚为 Y 的旧主库");
    let restored =
        SqliteMemoryRepository::open(root.path()).expect("旧库开放前应重放历史 subjects 闭包");
    for memory_id in ["closure-memory-a", "closure-memory-b", "closure-memory-c"] {
        assert!(
            restored
                .current(&scope_a, &MemoryId(memory_id.to_string()))
                .expect("旧库恢复后读取应成功")
                .is_none(),
            "闭包内任一历史形态都不得从旧备份复活"
        );
    }
    assert!(
        restored
            .current(&scope_b, &MemoryId("closure-memory-a".to_string()))
            .expect("旧库恢复后其他 Persona 读取应成功")
            .is_some(),
        "旧库恢复仍必须保持 Persona 隔离"
    );
}

#[test]
fn 派生canonicalization保留标点与emoji_单删不连删语义不同记忆() {
    let root = TestDirectory::new("derivation-canon-collision");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope_a = scope("persona-canon-a");
    let scope_b = scope("persona-canon-b");
    // 旧实现复用 FTS alphanumeric 归一化时，以下每对内容都会碰撞为同一派生键。
    let collision_pairs = [
        ("cpp", "用户喜欢 C++", "c", "用户喜欢 C"),
        ("aplus", "血型 A+", "a", "血型 A"),
        ("star", "🌟🌟🌟", "sparkle", "✨✨✨"),
    ];
    for (first_id, first_content, second_id, second_content) in &collision_pairs {
        for (memory_id, content) in [(first_id, first_content), (second_id, second_content)] {
            commit_create(
                &repository,
                &scope_a,
                &format!("canon-key-{memory_id}"),
                &format!("canon-conversation-{memory_id}"),
                &format!("canon-turn-{memory_id}"),
                &format!("canon-operation-{memory_id}"),
                &format!("canon-memory-{memory_id}"),
                &format!("canon-revision-{memory_id}"),
                content,
            );
        }
    }
    commit_create(
        &repository,
        &scope_b,
        "canon-key-b-cpp",
        "canon-conversation-b-cpp",
        "canon-turn-b-cpp",
        "canon-operation-b-cpp",
        "canon-memory-cpp",
        "canon-revision-b-cpp",
        "用户喜欢 C++",
    );

    let delete_cpp = confirmed_delete(
        &scope_a,
        "canon-delete-cpp",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("canon-memory-cpp".to_string()),
        },
    );
    let receipt = repository
        .delete_confirmed(&delete_cpp, repository.deletion_authority())
        .expect("单删 C++ 应成功");
    assert_eq!(
        receipt.deleted_memory_count, 1,
        "C++ 与 C 不得误并为同一派生而连删"
    );
    let delete_star = confirmed_delete(
        &scope_a,
        "canon-delete-star",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("canon-memory-star".to_string()),
        },
    );
    let receipt = repository
        .delete_confirmed(&delete_star, repository.deletion_authority())
        .expect("单删纯 emoji 应成功");
    assert_eq!(
        receipt.deleted_memory_count, 1,
        "不同纯 emoji 不得误并为同一派生而连删"
    );

    for (memory_id, label) in [
        ("canon-memory-c", "C 必须保留"),
        ("canon-memory-aplus", "A+ 必须保留"),
        ("canon-memory-a", "A 必须保留"),
        ("canon-memory-sparkle", "另一组 emoji 必须保留"),
    ] {
        assert!(
            repository
                .current(&scope_a, &MemoryId(memory_id.to_string()))
                .expect("幸存记忆 current 读取应成功")
                .is_some(),
            "{label}"
        );
    }
    assert!(
        repository
            .current(&scope_b, &MemoryId("canon-memory-cpp".to_string()))
            .expect("跨 Persona 读取应成功")
            .is_some(),
        "跨 Persona 同文记忆必须保持隔离"
    );
    let search_hits = repository
        .search_current_fts(&scope_a, "用户喜欢", 10)
        .expect("幸存记忆 search 应成功");
    assert_eq!(
        search_hits
            .iter()
            .map(|record| record.entry.memory_id.0.as_str())
            .collect::<Vec<_>>(),
        ["canon-memory-c"],
        "FTS 读取面不得连带隐藏或暴露错误记忆"
    );

    drop(repository);
    let repository = SqliteMemoryRepository::open(root.path()).expect("重启后应重新打开");
    for memory_id in [
        "canon-memory-c",
        "canon-memory-aplus",
        "canon-memory-a",
        "canon-memory-sparkle",
    ] {
        assert!(
            repository
                .current(&scope_a, &MemoryId(memory_id.to_string()))
                .expect("重启后幸存记忆读取应成功")
                .is_some(),
            "重启后 `{memory_id}` 必须仍可读"
        );
    }

    for (key_suffix, content, label) in [
        ("same", "用户喜欢 C++", "完全相同的已删除正文必须仍被阻断"),
        ("case", "用户喜欢 c++", "仅大小写差异的等价正文必须仍被阻断"),
    ] {
        let blocked = envelope(
            &scope_a,
            &format!("canon-rewrite-{key_suffix}"),
            &format!("canon-rewrite-conversation-{key_suffix}"),
            &format!("canon-rewrite-turn-{key_suffix}"),
            vec![staged_create(
                &scope_a,
                &format!("canon-rewrite-conversation-{key_suffix}"),
                &format!("canon-rewrite-turn-{key_suffix}"),
                &format!("canon-rewrite-operation-{key_suffix}"),
                &format!("canon-rewrite-memory-{key_suffix}"),
                &format!("canon-rewrite-revision-{key_suffix}"),
                content,
            )],
        );
        assert_eq!(
            repository
                .apply_committed_batch(&blocked, &AllowPolicy)
                .expect_err(label)
                .code(),
            MemoryErrorCode::SourceIneligible
        );
    }
    commit_create(
        &repository,
        &scope_a,
        "canon-rewrite-future",
        "canon-rewrite-conversation-future",
        "canon-rewrite-turn-future",
        "canon-rewrite-operation-future",
        "canon-rewrite-memory-future",
        "canon-rewrite-revision-future",
        "用户喜欢 C#",
    );
    assert!(
        repository
            .current(
                &scope_a,
                &MemoryId("canon-rewrite-memory-future".to_string())
            )
            .expect("未来非等价内容读取应成功")
            .is_some(),
        "未来非等价内容必须仍可写入"
    );
}

#[test]
fn source_turn_tombstone_阻断_repository_未来写入() {
    let root = TestDirectory::new("source-turn-write-block");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-source-turn-write");
    let request = MemoryDeletionAuthorityRequest::new(
        "source-turn-authority-record",
        BTreeSet::from([MemoryDeletionSubject::SourceTurn {
            persona_id: scope.persona_id().to_string(),
            conversation_id: "blocked-source-conversation".to_string(),
            turn_id: "blocked-source-turn".to_string(),
        }]),
        TIME_2,
    )
    .expect("SourceTurn 权威请求应有效");
    repository
        .deletion_authority()
        .record(&request)
        .expect("SourceTurn tombstone 应 durable");
    let blocked = envelope(
        &scope,
        "blocked-source-batch",
        "blocked-source-conversation",
        "blocked-source-turn",
        vec![staged_create(
            &scope,
            "blocked-source-conversation",
            "blocked-source-turn",
            "blocked-source-operation",
            "blocked-source-memory",
            "blocked-source-revision",
            "正文与历史删除内容完全不同也不得从被删来源重扫",
        )],
    );
    assert_eq!(
        repository
            .apply_committed_batch(&blocked, &AllowPolicy)
            .expect_err("SourceTurn tombstone 必须阻断 Repository 写入")
            .code(),
        MemoryErrorCode::SourceIneligible
    );
    assert!(
        repository
            .current(&scope, &MemoryId("blocked-source-memory".to_string()))
            .expect("阻断后读取应成功")
            .is_none()
    );
}

#[test]
fn fts_projection可重建且普通写入不强制_truncate_wal() {
    let root = TestDirectory::new("fts-rebuild");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-rebuild");
    let keeper = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应保持一个 WAL 连接");
    commit_create(
        &repository,
        &scope,
        "rebuild-create",
        "rebuild-conversation",
        "rebuild-turn",
        "rebuild-operation",
        "rebuild-memory",
        "rebuild-revision",
        "可以重建的海棠花事实",
    );
    assert!(
        root.path()
            .join("runtime/muse.sqlite-wal")
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0),
        "普通投影写入不应强制 TRUNCATE WAL"
    );
    drop(keeper);

    let connection = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应打开投影破坏连接");
    connection
        .execute(
            "DELETE FROM memory_search_projection
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-rebuild", "rebuild-memory"],
        )
        .expect("应模拟投影缺失");
    drop(connection);
    assert!(
        repository
            .search_current_fts(&scope, "海棠花", 10)
            .expect("投影缺失时查询应安全")
            .is_empty()
    );
    repository
        .rebuild_search_index()
        .expect("应从 current revision 重建 FTS");
    assert_eq!(
        repository
            .search_current_fts(&scope, "海棠花", 10)
            .expect("重建后查询应成功")
            .len(),
        1
    );
}

#[test]
fn 父版本_nfkc_前投影在无删除恢复时也会原子升级() {
    let root = TestDirectory::new("fts-normalization-upgrade");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-normalization-upgrade");
    let decomposed = "用户偏好Cafe\u{301}豆";
    let fullwidth = "用户偏好ＲＵＳＴ语言";
    commit_create(
        &repository,
        &scope,
        "normalization-create-decomposed",
        "normalization-conversation-decomposed",
        "normalization-turn-decomposed",
        "normalization-operation-decomposed",
        "normalization-memory-decomposed",
        "normalization-revision-decomposed",
        decomposed,
    );
    commit_create(
        &repository,
        &scope,
        "normalization-create-fullwidth",
        "normalization-conversation-fullwidth",
        "normalization-turn-fullwidth",
        "normalization-operation-fullwidth",
        "normalization-memory-fullwidth",
        "normalization-revision-fullwidth",
        fullwidth,
    );

    // 把投影降级成 2fd7309 的真实输出，并移除独立投影版本元数据；删除权威
    // 此时没有 pending event，升级必须仅由 projection version 触发。
    let connection = repository.open_connection().expect("应打开父版本夹具连接");
    connection
        .execute(
            "UPDATE memory_search_projection SET content = ?1
             WHERE persona_id = ?2 AND memory_id = ?3",
            params![
                normalize_projection_like_2fd7309(decomposed),
                scope.persona_id(),
                "normalization-memory-decomposed"
            ],
        )
        .expect("应写入分解态父版本投影");
    connection
        .execute(
            "UPDATE memory_search_projection SET content = ?1
             WHERE persona_id = ?2 AND memory_id = ?3",
            params![
                normalize_projection_like_2fd7309(fullwidth),
                scope.persona_id(),
                "normalization-memory-fullwidth"
            ],
        )
        .expect("应写入全角父版本投影");
    connection
        .execute_batch("DROP TABLE memory_search_projection_meta;")
        .expect("应模拟父版本缺少 projection metadata");
    drop(connection);
    assert!(
        repository
            .search_current_fts(&scope, "CAFÉ豆", 10)
            .expect("升级前分解态查询应安全")
            .is_empty(),
        "父版本投影不能命中新规范化查询"
    );
    assert!(
        repository
            .search_current_fts(&scope, "RUST语言", 10)
            .expect("升级前全角查询应安全")
            .is_empty(),
        "父版本投影不能命中新规范化查询"
    );
    drop(repository);

    let upgraded =
        SqliteMemoryRepository::open(root.path()).expect("无 pending 删除时也应完成投影版本升级");
    assert_eq!(
        upgraded
            .search_current_fts(&scope, "CAFÉ豆", 10)
            .expect("升级后组合态查询应成功")
            .len(),
        1
    );
    assert_eq!(
        upgraded
            .search_current_fts(&scope, "RUST语言", 10)
            .expect("升级后半角查询应成功")
            .len(),
        1
    );
    let metadata = upgraded.open_connection().expect("应读取投影版本元数据");
    assert_eq!(
        metadata
            .query_row(
                "SELECT normalization_version
                 FROM memory_search_projection_meta WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("升级后应记录投影规范化版本"),
        2
    );
}

#[test]
fn generic_authority_record_只在新_revision_时恢复一次() {
    let root = TestDirectory::new("generic-authority-record");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-generic-record");
    commit_create(
        &repository,
        &scope,
        "generic-create",
        "generic-conversation",
        "generic-turn",
        "generic-operation",
        "generic-keeper",
        "generic-revision",
        "generic record 不应每次启动重建投影",
    );
    let generic = MemoryDeletionAuthorityRequest::new(
        "generic-deletion-record",
        BTreeSet::from([MemoryDeletionSubject::Memory {
            persona_id: scope.persona_id().to_string(),
            memory_id: MemoryId("generic-nonexistent".to_string()),
        }]),
        TIME_3,
    )
    .expect("generic authority 请求应有效");
    repository
        .deletion_authority()
        .record(&generic)
        .expect("generic authority record 应 durable");
    drop(repository);

    let first_reopen =
        SqliteMemoryRepository::open(root.path()).expect("新 authority revision 应恢复一次");
    assert_eq!(
        first_reopen
            .search_current_fts(&scope, "每次启动", 10)
            .expect("首次恢复后投影应完整")
            .len(),
        1
    );
    let projection = first_reopen.open_connection().expect("应打开投影测试连接");
    projection
        .execute(
            "DELETE FROM memory_search_projection
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-generic-record", "generic-keeper"],
        )
        .expect("应模拟恢复后的独立投影缺失");
    drop(projection);
    drop(first_reopen);

    let second_reopen =
        SqliteMemoryRepository::open(root.path()).expect("generic event 不应永久 pending");
    assert!(
        second_reopen
            .search_current_fts(&scope, "每次启动", 10)
            .expect("二次启动查询应成功")
            .is_empty(),
        "revision 已应用且无 cleanup intent 的 generic event 不得再次重建"
    );
}

#[test]
fn checkpoint_busy_不签删除完成且重试可收口() {
    let root = TestDirectory::new("checkpoint-busy");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-checkpoint");
    let sentinel = "checkpoint 忙时不能签收的正文-SENTINEL-8842";
    commit_create(
        &repository,
        &scope,
        "checkpoint-create",
        "checkpoint-conversation",
        "checkpoint-turn",
        "checkpoint-operation",
        "checkpoint-memory",
        "checkpoint-revision",
        sentinel,
    );
    let reader = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应打开并发 reader");
    reader.execute_batch("BEGIN").expect("应开始读事务");
    let _: String = reader
        .query_row(
            "SELECT content FROM memory_revision
             WHERE persona_id = ?1 AND memory_id = ?2",
            params!["persona-checkpoint", "checkpoint-memory"],
            |row| row.get(0),
        )
        .expect("应固定 WAL reader 快照");
    let request = confirmed_delete(
        &scope,
        "checkpoint-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("checkpoint-memory".to_string()),
        },
    );
    let error = repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect_err("checkpoint busy 不得签完整成功");
    assert_eq!(error.code(), MemoryErrorCode::DeletionIncomplete);
    let main = crate::app::storage::open_initialized_runtime_database(root.path())
        .expect("应打开 checkpoint 中间态主库");
    let (entry_count, anchor_revision): (i64, i64) = main
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM memory_entry
                 WHERE persona_id = 'persona-checkpoint'
                   AND memory_id = 'checkpoint-memory'),
                (SELECT last_applied_revision
                 FROM memory_authority_anchor WHERE singleton = 1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("应读取 checkpoint 中间态");
    assert_eq!(entry_count, 0, "主库事务已经删除正文");
    drop(main);
    let authority = Connection::open(root.path().join("privacy/memory-deletion-authority.sqlite"))
        .expect("应打开 checkpoint 中间态权威");
    let (authority_revision, cleanup_completed_at, target_count, deleted_count): (
        i64,
        Option<String>,
        Option<i64>,
        Option<i64>,
    ) = authority
        .query_row(
            "SELECT authority_revision, cleanup_completed_at,
                    target_memory_count, deleted_memory_count
             FROM deletion_event
             WHERE deletion_id = 'checkpoint-delete'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("应读取 checkpoint 中间态权威");
    assert_eq!(anchor_revision, authority_revision);
    assert_eq!(cleanup_completed_at, None);
    assert_eq!(target_count, Some(1));
    assert_eq!(deleted_count, None);
    drop(authority);
    assert!(
        repository
            .current(&scope, &MemoryId("checkpoint-memory".to_string()))
            .expect("checkpoint 中间态读取应 fail closed")
            .is_none()
    );

    reader.execute_batch("ROLLBACK").expect("应释放 reader");
    drop(reader);
    let receipt = repository
        .delete_confirmed(&request, repository.deletion_authority())
        .expect("原 deletion_id 应从 authority 收据继续完成");
    assert_eq!(
        repository
            .delete_confirmed(&request, repository.deletion_authority())
            .expect("完成后的 deletion_id 应严格幂等"),
        receipt
    );
    let authority = Connection::open(root.path().join("privacy/memory-deletion-authority.sqlite"))
        .expect("应打开完成态权威");
    let completed: (bool, Option<i64>) = authority
        .query_row(
            "SELECT cleanup_completed_at IS NOT NULL, deleted_memory_count
             FROM deletion_event
             WHERE deletion_id = 'checkpoint-delete'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("应读取完成态权威");
    assert_eq!(completed, (true, Some(1)));
    drop(authority);
    drop(repository);
    let normalized = normalize_memory_fts_query(sentinel).expect("sentinel 应可规范化");
    for path in [
        root.path().join("runtime/muse.sqlite"),
        root.path().join("runtime/muse.sqlite-wal"),
        root.path().join("runtime/muse.sqlite-shm"),
    ] {
        assert!(!file_contains(&path, sentinel.as_bytes()));
        assert!(!file_contains(&path, normalized.as_bytes()));
    }
}

#[test]
fn 两个_repository_实例并发_update_delete_无反锁且最终不复活() {
    let root = TestDirectory::new("concurrent-delete");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-concurrent-delete");
    commit_create(
        &repository,
        &scope,
        "concurrent-create",
        "concurrent-conversation-0",
        "concurrent-turn-0",
        "concurrent-operation-0",
        "concurrent-memory",
        "concurrent-revision-0",
        "并发删除前的事实",
    );
    drop(repository);
    let updater = SqliteMemoryRepository::open(root.path()).expect("应打开 updater");
    let deleter = SqliteMemoryRepository::open(root.path()).expect("应打开 deleter");
    let update = envelope(
        &scope,
        "concurrent-update",
        "concurrent-conversation-1",
        "concurrent-turn-1",
        vec![staged_change(
            &scope,
            "concurrent-conversation-1",
            "concurrent-turn-1",
            "concurrent-operation-1",
            "concurrent-memory",
            "concurrent-revision-0",
            "concurrent-revision-1",
            "并发更新后的事实",
            false,
        )],
    );
    let delete = confirmed_delete(
        &scope,
        "concurrent-delete",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("concurrent-memory".to_string()),
        },
    );
    let barrier = Arc::new(Barrier::new(3));
    let update_barrier = Arc::clone(&barrier);
    let update_handle = thread::spawn(move || {
        update_barrier.wait();
        updater.apply_committed_batch(&update, &AllowPolicy)
    });
    let delete_barrier = Arc::clone(&barrier);
    let delete_handle = thread::spawn(move || {
        delete_barrier.wait();
        deleter.delete_confirmed(&delete, deleter.deletion_authority())
    });
    barrier.wait();
    let update_result = update_handle.join().expect("update 线程不应 panic");
    let delete_result = delete_handle.join().expect("delete 线程不应 panic");
    assert!(delete_result.is_ok(), "删除必须最终成功：{delete_result:?}");
    if let Err(error) = update_result {
        assert!(matches!(
            error.code(),
            MemoryErrorCode::SourceIneligible | MemoryErrorCode::MemoryNotFound
        ));
    }
    let reopened = SqliteMemoryRepository::open(root.path()).expect("重启应成功");
    assert!(
        reopened
            .current(&scope, &MemoryId("concurrent-memory".to_string()))
            .expect("最终读取应成功")
            .is_none()
    );
}

#[test]
fn authority_commit_重锁窗口允许第二删除先收口且不形成_abba() {
    let root = TestDirectory::new("authority-commit-window");
    let repository = SqliteMemoryRepository::open(root.path()).expect("应打开 Repository");
    let scope = scope("persona-authority-window");
    commit_create(
        &repository,
        &scope,
        "window-create-1",
        "window-conversation-1",
        "window-turn-1",
        "window-operation-1",
        "window-memory-1",
        "window-revision-1",
        "第一个并发删除事实",
    );
    commit_create(
        &repository,
        &scope,
        "window-create-2",
        "window-conversation-2",
        "window-turn-2",
        "window-operation-2",
        "window-memory-2",
        "window-revision-2",
        "第二个并发删除事实",
    );
    drop(repository);

    let first_repository = SqliteMemoryRepository::open(root.path()).expect("应打开第一个删除实例");
    let second_repository =
        SqliteMemoryRepository::open(root.path()).expect("应打开第二个删除实例");
    let first_request = confirmed_delete(
        &scope,
        "window-delete-1",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("window-memory-1".to_string()),
        },
    );
    let second_request = confirmed_delete(
        &scope,
        "window-delete-2",
        MemoryDeleteParams::Memory {
            memory_id: MemoryId("window-memory-2".to_string()),
        },
    );
    let (window_entered, resume_first) = pause_after_authority_commit_for_test(
        root.path().join("privacy/memory-deletion-authority.sqlite"),
    );
    let first_handle = thread::spawn(move || {
        first_repository.delete_confirmed(&first_request, first_repository.deletion_authority())
    });
    window_entered
        .recv_timeout(Duration::from_secs(3))
        .expect("第一个删除应进入 authority COMMIT/relock 窗口");

    let (second_sender, second_receiver) = mpsc::sync_channel(1);
    let second_handle = thread::spawn(move || {
        let result = second_repository
            .delete_confirmed(&second_request, second_repository.deletion_authority());
        second_sender.send(result).expect("应发送第二个删除结果");
    });
    let second_result = match second_receiver.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => result,
        Err(error) => {
            resume_first.send(()).expect("超时时仍应释放第一个删除窗口");
            let _ = first_handle.join();
            let _ = second_handle.join();
            panic!("第二个删除在首个 authority 重锁前未收口，可能形成 ABBA：{error}");
        }
    };
    assert!(
        second_result.is_ok(),
        "第二个删除应在第一个恢复前完成：{second_result:?}"
    );
    resume_first.send(()).expect("应释放第一个删除窗口");
    let first_result = first_handle.join().expect("第一个删除线程不应 panic");
    second_handle.join().expect("第二个删除线程不应 panic");
    assert!(
        first_result.is_ok(),
        "第一个删除应幂等收口：{first_result:?}"
    );

    let reopened = SqliteMemoryRepository::open(root.path()).expect("并发删除后重启应成功");
    for memory_id in ["window-memory-1", "window-memory-2"] {
        assert!(
            reopened
                .current(&scope, &MemoryId(memory_id.to_string()))
                .expect("并发删除后读取应成功")
                .is_none()
        );
    }
}
