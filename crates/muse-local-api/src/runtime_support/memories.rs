//! Persona 长期记忆管理 API 的适配实现。
//!
//! 所有读取经 `MemoryRetriever` 与 `MemoryRepository::current` 端口，写操作统一
//! 交给 `MemoryManagementCommands`，由集成层持久化客户端 operation 身份并复用
//! Repository 的敏感双门、重要程度与删除权威能力。
//! 生产启动由 `build_sqlite_memory_services` 构造共享服务束；未成功接线时管理
//! 路由一律返回 503 与对应 `memory_*` 稳定码。

use axum::extract::rejection::{JsonRejection, QueryRejection};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use muse_core::app::memory_safety::DeterministicMemorySensitivityPolicy;
use muse_core::app::memory_storage::{
    MAX_REVISION_HISTORY_PAGE_SIZE, MEMORY_QUERY_TURN_MAX_CALLS, SqliteMemoryRepository,
    SqliteMemoryRetriever,
};
use muse_core::domain::memory::{
    ConfirmedMemoryDeleteRequest, MemoryCursor, MemoryDeleteConfirmation,
    MemoryDeleteConfirmationSource, MemoryDeleteParams, MemoryDeleteReceipt, MemoryError,
    MemoryErrorCode, MemoryId, MemoryImportanceAdjustment, MemoryImportanceAdjustmentReceipt,
    MemoryManagementAuthorization, MemoryManagementBinding, MemoryManagementContentMutation,
    MemoryManagementContentParams, MemoryMutationReceipt, MemoryPersonaScope,
    MemoryQueryPageReceipt, MemoryQueryParams, MemoryRepository, MemoryRetrievalFilters,
    MemoryRetrievalRequest, MemoryRetrievalTurn, MemoryRetriever, MemoryRevision, MemoryRevisionId,
    MemorySensitivityPolicy,
};
use std::num::NonZeroU32;
use std::path::Path as FsPath;

use super::*;
use crate::state::MemoryRuntimeServices;

/// 管理面只读审计端口（最小签名）。
///
/// 生产适配器复用服务束中的 `SqliteMemoryRepository` 只读方法实现本 trait，
/// 不旁路直写表。
pub trait MemoryManagementAudit: Send + Sync {
    /// 返回指定记忆的完整 revision 链（含 corrected 审计条目，仅限管理入口）；
    /// 记忆不存在时必须返回 `MemoryErrorCode::MemoryNotFound`。
    fn revision_history(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
    ) -> Result<Vec<MemoryRevision>, MemoryError>;

    /// 统计指定 Persona 当前有效记忆条数，供删除影响评估。
    fn active_memory_count(&self, scope: &MemoryPersonaScope) -> Result<u64, MemoryError>;
}

/// Persona 记忆管理写命令端口。
///
/// `operation_id` 是客户端操作的持久幂等身份：实现方必须把首次绑定的运行时
/// 身份、目标与收据持久化，同一 Persona 下相同 ID、相同请求的重放必须返回原
/// 收据，相同 ID、不同请求必须稳定拒绝。尤其是 `PersonaAll` 删除不得在重放时
/// 重新选取 subjects，否则会误删首次成功之后新创建的记忆。
pub trait MemoryManagementCommands: Send + Sync {
    fn create(
        &self,
        scope: &MemoryPersonaScope,
        operation_id: &str,
        request: &MemoryCreateRequest,
    ) -> Result<MemoryMutationReceipt, MemoryError>;

    fn correct(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        operation_id: &str,
        request: &MemoryCorrectRequest,
    ) -> Result<MemoryMutationReceipt, MemoryError>;

    fn adjust_importance(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        operation_id: &str,
        request: &MemoryImportanceAdjustRequest,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError>;

    fn delete(
        &self,
        scope: &MemoryPersonaScope,
        operation_id: &str,
        params: &MemoryDeleteParams,
    ) -> Result<MemoryDeleteReceipt, MemoryError>;
}

/// 记忆管理 API 的运行时服务接缝；真实实例由生产启动流程注入。
///
/// Repository 使用具体 SQLite 实现供详情读取；写命令由 `commands` 负责稳定
/// operation 绑定与 Repository 调用，防止 HTTP 重试重新生成领域身份。
pub struct MemoryServices {
    pub repository: Arc<SqliteMemoryRepository>,
    pub retriever: Arc<dyn MemoryRetriever>,
    pub commands: Arc<dyn MemoryManagementCommands>,
    pub audit: Arc<dyn MemoryManagementAudit>,
}

const MEMORY_MANAGEMENT_DELETE_CONFIRMATION_TTL_MINUTES: i64 = 5;
const MAX_MEMORY_MANAGEMENT_HISTORY_PAGES: usize = 8;

/// 生产管理命令与审计适配器；所有路径共享同一个 SQLite Repository。
struct SqliteMemoryManagementAdapter {
    repository: Arc<SqliteMemoryRepository>,
    sensitivity: Arc<dyn MemorySensitivityPolicy>,
}

impl SqliteMemoryManagementAdapter {
    fn new(
        repository: Arc<SqliteMemoryRepository>,
        sensitivity: Arc<dyn MemorySensitivityPolicy>,
    ) -> Self {
        Self {
            repository,
            sensitivity,
        }
    }

    fn binding(
        scope: &MemoryPersonaScope,
        operation_id: &str,
    ) -> Result<MemoryManagementBinding, MemoryError> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
        let authorization =
            MemoryManagementAuthorization::from_runtime(scope.clone(), operation_id, now.clone())?;
        MemoryManagementBinding::bind(authorization, operation_id, now.clone(), now.clone(), now)
    }

    fn assigned_id(kind: &str, scope: &MemoryPersonaScope, operation_id: &str) -> String {
        let mut digest = Sha256::new();
        for value in [
            kind.as_bytes(),
            scope.persona_id().as_bytes(),
            operation_id.as_bytes(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        let digest = digest.finalize();
        let mut encoded = String::with_capacity(digest.len() * 2);
        use std::fmt::Write as _;
        for byte in digest {
            write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
        }
        format!("{kind}-{encoded}")
    }
}

impl MemoryManagementCommands for SqliteMemoryManagementAdapter {
    fn create(
        &self,
        scope: &MemoryPersonaScope,
        operation_id: &str,
        request: &MemoryCreateRequest,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        let mutation = MemoryManagementContentMutation::bind(
            MemoryManagementContentParams::Create {
                category: request.category,
                content: request.content.clone(),
                importance: request.importance,
                event_time: request.event_time.clone(),
                change_reason: request.change_reason.clone(),
            },
            Self::binding(scope, operation_id)?,
            MemoryId(Self::assigned_id("memory", scope, operation_id)),
            MemoryRevisionId(Self::assigned_id("memory-revision", scope, operation_id)),
            self.sensitivity.as_ref(),
        )?;
        self.repository
            .apply_management_content_mutation(&mutation, self.sensitivity.as_ref())
    }

    fn correct(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        operation_id: &str,
        request: &MemoryCorrectRequest,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        let mutation = MemoryManagementContentMutation::bind(
            MemoryManagementContentParams::Correct {
                memory_id: memory_id.clone(),
                expected_revision_id: request.expected_revision_id.clone(),
                category: request.category,
                content: request.content.clone(),
                event_time: request.event_time.clone(),
                change_reason: request.change_reason.clone(),
            },
            Self::binding(scope, operation_id)?,
            memory_id.clone(),
            MemoryRevisionId(Self::assigned_id("memory-revision", scope, operation_id)),
            self.sensitivity.as_ref(),
        )?;
        self.repository
            .apply_management_content_mutation(&mutation, self.sensitivity.as_ref())
    }

    fn adjust_importance(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        operation_id: &str,
        request: &MemoryImportanceAdjustRequest,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError> {
        let adjustment = MemoryImportanceAdjustment::bind(
            Self::binding(scope, operation_id)?,
            memory_id.clone(),
            request.expected_revision_id.clone(),
            request.expected_importance,
            request.importance,
        )?;
        self.repository.adjust_importance(&adjustment)
    }

    fn delete(
        &self,
        scope: &MemoryPersonaScope,
        operation_id: &str,
        params: &MemoryDeleteParams,
    ) -> Result<MemoryDeleteReceipt, MemoryError> {
        let confirmed_at = Utc::now();
        let expires_at = confirmed_at
            + ChronoDuration::minutes(MEMORY_MANAGEMENT_DELETE_CONFIRMATION_TTL_MINUTES);
        let confirmation = MemoryDeleteConfirmation::new(
            operation_id,
            scope,
            params,
            confirmed_at.to_rfc3339_opts(SecondsFormat::Micros, true),
            expires_at.to_rfc3339_opts(SecondsFormat::Micros, true),
            MemoryDeleteConfirmationSource::PersonaManagement {
                action_id: operation_id.to_string(),
            },
        )?;
        let request =
            ConfirmedMemoryDeleteRequest::bind(params.clone(), scope.clone(), confirmation)?;
        self.repository
            .delete_confirmed(&request, self.repository.deletion_authority())
    }
}

impl MemoryManagementAudit for SqliteMemoryManagementAdapter {
    fn revision_history(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
    ) -> Result<Vec<MemoryRevision>, MemoryError> {
        let mut revisions = Vec::new();
        let mut cursor = None;
        for _ in 0..MAX_MEMORY_MANAGEMENT_HISTORY_PAGES {
            let page = self.repository.revision_history(
                scope,
                memory_id,
                cursor.as_ref(),
                MAX_REVISION_HISTORY_PAGE_SIZE,
            )?;
            revisions.extend(page.revisions);
            let Some(next_cursor) = page.next_cursor else {
                return Ok(revisions);
            };
            cursor = Some(next_cursor);
        }
        Err(MemoryError::new(MemoryErrorCode::QueryBudgetExceeded))
    }

    fn active_memory_count(&self, scope: &MemoryPersonaScope) -> Result<u64, MemoryError> {
        self.repository.active_memory_count(scope)
    }
}

/// 构建 Tool、管理 API 与 Persona 生命周期共用的生产 SQLite 服务束。
pub(crate) fn build_sqlite_memory_services(
    data_dir: &FsPath,
) -> Result<(MemoryRuntimeServices, MemoryServices), MemoryError> {
    let repository = Arc::new(SqliteMemoryRepository::open(data_dir)?);
    let retriever = Arc::new(SqliteMemoryRetriever::new(Arc::clone(&repository)));
    let sensitivity: Arc<dyn MemorySensitivityPolicy> =
        Arc::new(DeterministicMemorySensitivityPolicy::new());
    let management = Arc::new(SqliteMemoryManagementAdapter::new(
        Arc::clone(&repository),
        Arc::clone(&sensitivity),
    ));
    let query_call_budget = NonZeroU32::new(
        u32::try_from(MEMORY_QUERY_TURN_MAX_CALLS)
            .map_err(|_| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?,
    )
    .ok_or_else(|| MemoryError::new(MemoryErrorCode::RepositoryUnavailable))?;

    let runtime_services = MemoryRuntimeServices {
        retriever: retriever.clone(),
        repository: repository.clone(),
        sensitivity,
        deletion_authority: repository.shared_deletion_authority(),
        query_call_budget,
    };
    let management_services = MemoryServices {
        repository,
        retriever,
        commands: management.clone(),
        audit: management,
    };
    Ok((runtime_services, management_services))
}

// 管理路由目前共用进程级槽位；生产启动与 AppState 使用同一次服务束构造结果，
// 因而不会产生第二个 Repository 或删除权威实例。未注入时稳定返回 503。
static MEMORY_SERVICES: std::sync::RwLock<Option<Arc<MemoryServices>>> =
    std::sync::RwLock::new(None);

/// 集成分支接线入口：启动时注入真实记忆服务实例。
pub fn install_memory_services(services: MemoryServices) {
    let mut slot = MEMORY_SERVICES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = Some(Arc::new(services));
}

pub(super) fn memory_services() -> Option<Arc<MemoryServices>> {
    MEMORY_SERVICES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(super) fn require_memory_services()
-> Result<Arc<MemoryServices>, (StatusCode, Json<ErrorResponse>)> {
    memory_services().ok_or_else(|| {
        memory_error_response(MemoryError::new(MemoryErrorCode::RepositoryUnavailable))
    })
}

fn memory_json_rejection(_: JsonRejection) -> (StatusCode, Json<ErrorResponse>) {
    memory_error_response(MemoryError::new(MemoryErrorCode::InvalidRequest))
}

fn memory_query_rejection(_: QueryRejection) -> (StatusCode, Json<ErrorResponse>) {
    memory_error_response(MemoryError::new(MemoryErrorCode::InvalidRequest))
}

/// MemoryError 到稳定 HTTP 响应的唯一映射；错误体前缀即 16 个 memory_* 稳定码。
pub(super) fn memory_error_response(error: MemoryError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match error.code() {
        MemoryErrorCode::InvalidRequest
        | MemoryErrorCode::InvalidCursor
        | MemoryErrorCode::QueryRejected => StatusCode::BAD_REQUEST,
        MemoryErrorCode::CursorExpired => StatusCode::GONE,
        MemoryErrorCode::InvalidStateTransition
        | MemoryErrorCode::RevisionConflict
        | MemoryErrorCode::DeleteConfirmationRequired => StatusCode::CONFLICT,
        MemoryErrorCode::MemoryNotFound => StatusCode::NOT_FOUND,
        MemoryErrorCode::PersonaScopeMismatch | MemoryErrorCode::SourceIneligible => {
            StatusCode::FORBIDDEN
        }
        MemoryErrorCode::SensitiveContentRejected => StatusCode::UNPROCESSABLE_ENTITY,
        MemoryErrorCode::QueryBudgetExceeded => StatusCode::TOO_MANY_REQUESTS,
        MemoryErrorCode::SensitivityUnavailable
        | MemoryErrorCode::DeletionAuthorityUnavailable
        | MemoryErrorCode::RepositoryUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        MemoryErrorCode::DeletionIncomplete => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorResponse {
            error: format!("{}：{error}", error.stable_code()),
        }),
    )
}

/// Persona scope 只能从路径 id 绑定；不存在的角色一律 404，客户端无法跨 Persona。
async fn require_existing_persona_scope(
    state: &Arc<AppState>,
    persona_id: &str,
) -> Result<MemoryPersonaScope, (StatusCode, Json<ErrorResponse>)> {
    {
        let personas = state.personas.lock().await;
        if personas.get(persona_id).is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("角色 `{persona_id}` 不存在"),
                }),
            ));
        }
    }
    MemoryPersonaScope::new(persona_id).map_err(memory_error_response)
}

fn resolve_operation_id(
    operation_id: Option<&str>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let operation_id = operation_id
        .map(str::to_string)
        .unwrap_or_else(|| next_runtime_id("memory-op"));
    if operation_id.is_empty()
        || operation_id.len() > 128
        || !operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(memory_error_response(MemoryError::new(
            MemoryErrorCode::InvalidRequest,
        )));
    }
    Ok(operation_id)
}

/// 清空指定 Persona 的全部记忆；handle_delete_persona 与全清路由共用同一删除权威链路。
pub(super) fn delete_all_persona_memories(
    services: &MemoryServices,
    persona_id: &str,
    operation_id: &str,
) -> Result<MemoryDeleteReceipt, MemoryError> {
    let scope = MemoryPersonaScope::new(persona_id)?;
    services
        .commands
        .delete(&scope, operation_id, &MemoryDeleteParams::PersonaAll)
}

/// 删除影响评估的记忆条数；只在已接线时可用。
pub(super) fn persona_memory_count(
    services: &MemoryServices,
    persona_id: &str,
) -> Result<u64, MemoryError> {
    services
        .audit
        .active_memory_count(&MemoryPersonaScope::new(persona_id)?)
}

/// 查询当前有效记忆列表（query/category/importance/cursor 筛选）。
pub(crate) async fn handle_persona_memories(
    Path(persona_id): Path<String>,
    query: Result<Query<MemoryListQuery>, QueryRejection>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MemoryQueryPageReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let Query(query) = query.map_err(memory_query_rejection)?;
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let cursor = query
        .cursor
        .map(MemoryCursor::from_runtime)
        .transpose()
        .map_err(memory_error_response)?;
    let params = MemoryQueryParams {
        query: query.query,
        limit: None,
        cursor,
        as_of: None,
        memory_id: None,
        include_history: false,
    };
    // 管理页不是聊天 Turn；使用独立稳定 owner 让已签名游标可跨 HTTP 翻页，
    // Persona、查询和筛选条件仍由 Retriever 写入游标签名并严格绑定。
    let turn = MemoryRetrievalTurn::from_runtime(
        "persona-memory-management",
        "persona-memory-management-v1",
    )
    .map_err(memory_error_response)?;
    let filters = MemoryRetrievalFilters::new(query.category, query.importance);
    let request = MemoryRetrievalRequest::bind(params, scope, turn, filters)
        .map_err(memory_error_response)?;
    let receipt = services
        .retriever
        .retrieve(&request)
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

/// 查询单条记忆详情与来源跳转索引。
pub(crate) async fn handle_get_persona_memory(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MemoryDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let record = services
        .repository
        .current(&scope, &MemoryId(memory_id.clone()))
        .map_err(memory_error_response)?
        .ok_or_else(|| memory_error_response(MemoryError::new(MemoryErrorCode::MemoryNotFound)))?;
    Ok(Json(MemoryDetailResponse {
        source_conversation_id: record
            .current_revision
            .source
            .conversation_id()
            .map(str::to_string),
        source_turn_id: record.current_revision.source.turn_id().map(str::to_string),
        entry: record.entry,
        current_revision: record.current_revision.into(),
    }))
}

/// 查询版本历史；corrected 审计条目只允许该管理入口读取。
pub(crate) async fn handle_persona_memory_history(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MemoryHistoryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let memory_id = MemoryId(memory_id);
    let revisions = services
        .audit
        .revision_history(&scope, &memory_id)
        .map_err(memory_error_response)?;
    Ok(Json(MemoryHistoryResponse {
        memory_id,
        revisions: revisions.into_iter().map(Into::into).collect(),
    }))
}

/// 手工新增记忆。
pub(crate) async fn handle_create_persona_memory(
    Path(persona_id): Path<String>,
    State(state): State<Arc<AppState>>,
    request: Result<Json<MemoryCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MemoryMutationReceipt>), (StatusCode, Json<ErrorResponse>)> {
    let Json(request) = request.map_err(memory_json_rejection)?;
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let operation_id = resolve_operation_id(request.operation_id.as_deref())?;
    let receipt = services
        .commands
        .create(&scope, &operation_id, &request)
        .map_err(memory_error_response)?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

/// 纠正记忆；expected_revision_id 并发校验失败返回 409 memory_revision_conflict。
pub(crate) async fn handle_correct_persona_memory(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    request: Result<Json<MemoryCorrectRequest>, JsonRejection>,
) -> Result<Json<MemoryMutationReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let Json(request) = request.map_err(memory_json_rejection)?;
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let memory_id = MemoryId(memory_id);
    let operation_id = resolve_operation_id(request.operation_id.as_deref())?;
    let receipt = services
        .commands
        .correct(&scope, &memory_id, &operation_id, &request)
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

/// 调整重要程度；只更新逻辑 entry，不创建内容 revision。
pub(crate) async fn handle_adjust_persona_memory_importance(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    request: Result<Json<MemoryImportanceAdjustRequest>, JsonRejection>,
) -> Result<Json<MemoryImportanceAdjustmentReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let Json(request) = request.map_err(memory_json_rejection)?;
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let memory_id = MemoryId(memory_id);
    let operation_id = resolve_operation_id(request.operation_id.as_deref())?;
    let receipt = services
        .commands
        .adjust_importance(&scope, &memory_id, &operation_id, &request)
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

/// 删除单条记忆；确认证据由服务端以 Persona 管理来源构造。
pub(crate) async fn handle_delete_persona_memory(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    request: Result<Json<MemoryDeleteRequest>, JsonRejection>,
) -> Result<Json<MemoryDeleteReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let Json(request) = request.map_err(memory_json_rejection)?;
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let operation_id = resolve_operation_id(Some(&request.operation_id))?;
    let receipt = services
        .commands
        .delete(
            &scope,
            &operation_id,
            &MemoryDeleteParams::Memory {
                memory_id: MemoryId(memory_id),
            },
        )
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

/// 清空该 Persona 的全部记忆。
pub(crate) async fn handle_clear_persona_memories(
    Path(persona_id): Path<String>,
    State(state): State<Arc<AppState>>,
    request: Result<Json<MemoryDeleteRequest>, JsonRejection>,
) -> Result<Json<MemoryDeleteReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let Json(request) = request.map_err(memory_json_rejection)?;
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let operation_id = resolve_operation_id(Some(&request.operation_id))?;
    let receipt = services
        .commands
        .delete(&scope, &operation_id, &MemoryDeleteParams::PersonaAll)
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

#[cfg(test)]
static MEMORY_SERVICES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 串行化所有触碰进程级服务槽位的测试，避免并行用例互相污染。
#[cfg(test)]
pub(crate) async fn memory_services_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = MEMORY_SERVICES_TEST_LOCK.lock().await;
    clear_memory_services_for_test();
    guard
}

#[cfg(test)]
pub(crate) fn clear_memory_services_for_test() {
    let mut slot = MEMORY_SERVICES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = None;
}

#[cfg(test)]
mod production_wiring_tests {
    use super::*;
    use muse_core::domain::memory::{MemoryCategory, MemoryImportance};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "muse-memory-production-wiring-{}-{sequence}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn 生产服务束共享_repository_检索器_删除权威并支持稳定重试() {
        let directory = TestDirectory::new();
        let (runtime, management) =
            build_sqlite_memory_services(&directory.0).expect("生产记忆服务束应可构建");
        assert_eq!(
            runtime.query_call_budget.get(),
            u32::try_from(MEMORY_QUERY_TURN_MAX_CALLS).expect("查询预算应可转为 u32")
        );

        let scope = MemoryPersonaScope::new("persona-production-wiring")
            .expect("测试 Persona scope 应有效");
        let create = MemoryCreateRequest {
            category: MemoryCategory::UserPreference,
            content: "用户喜欢在周末散步".to_string(),
            importance: MemoryImportance::Normal,
            event_time: None,
            change_reason: "用户在管理页明确新增".to_string(),
            operation_id: Some("production-create-1".to_string()),
        };
        let first = management
            .commands
            .create(&scope, "production-create-1", &create)
            .expect("管理创建应通过真实 Repository");
        let replay = management
            .commands
            .create(&scope, "production-create-1", &create)
            .expect("相同管理创建重试应幂等");
        assert_eq!(first, replay);
        assert!(
            runtime
                .repository
                .current(&scope, &first.memory_id)
                .expect("Tool 运行时应读取同一 Repository")
                .is_some()
        );
        assert_eq!(
            management
                .audit
                .active_memory_count(&scope)
                .expect("管理审计应读取同一 Repository"),
            1
        );

        let delete_params = MemoryDeleteParams::Memory {
            memory_id: first.memory_id.clone(),
        };
        let deleted = management
            .commands
            .delete(&scope, "production-delete-1", &delete_params)
            .expect("管理删除应使用 Repository 内部权威");
        let delete_replay = management
            .commands
            .delete(&scope, "production-delete-1", &delete_params)
            .expect("相同管理删除重试应幂等");
        assert_eq!(deleted, delete_replay);
        assert!(
            runtime
                .repository
                .current(&scope, &first.memory_id)
                .expect("删除后 Tool 运行时读取应成功")
                .is_none()
        );
    }
}
