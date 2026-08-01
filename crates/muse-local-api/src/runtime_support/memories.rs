//! Persona 长期记忆管理 API 的适配实现。
//!
//! 所有读取经 `MemoryRetriever` 与 `MemoryRepository::current` 端口，写操作统一
//! 交给 `MemoryManagementCommands`，由集成层持久化客户端 operation 身份并复用
//! Repository 的敏感双门、重要程度与删除权威能力。
//! 本工作树未接线真实实例，管理路由一律返回 503 与对应 `memory_*` 稳定码；
//! 真实实例由协调者在临时集成分支通过 `install_memory_services` 注入。

use axum::extract::rejection::{JsonRejection, QueryRejection};
use muse_core::app::memory_storage::SqliteMemoryRepository;
use muse_core::domain::memory::{
    MemoryCursor, MemoryDeleteParams, MemoryDeleteReceipt, MemoryError, MemoryErrorCode, MemoryId,
    MemoryImportanceAdjustmentReceipt, MemoryMutationReceipt, MemoryPersonaScope,
    MemoryQueryPageReceipt, MemoryQueryParams, MemoryRepository, MemoryRetrievalRequest,
    MemoryRetriever, MemoryRevision,
};

use super::*;

/// 管理面只读审计端口（最小签名）。
///
/// 协调者在集成分支用 ② 提供的 `SqliteMemoryRepository` 只读方法编写 adapter
/// 实现本 trait；本切片不旁路直写表。
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

/// 记忆管理 API 的运行时服务接缝；真实实例由协调者在集成分支启动流程注入。
///
/// Repository 使用具体 SQLite 实现供详情读取；写命令由 `commands` 负责稳定
/// operation 绑定与 Repository 调用，防止 HTTP 重试重新生成领域身份。
pub struct MemoryServices {
    pub repository: Arc<SqliteMemoryRepository>,
    pub retriever: Arc<dyn MemoryRetriever>,
    pub commands: Arc<dyn MemoryManagementCommands>,
    pub audit: Arc<dyn MemoryManagementAudit>,
}

// `AppState` 归切片①修改，本切片不能追加字段；服务接缝因此挂在进程级槽位上，
// 集成分支在启动流程注入一次。未注入即“未接线”，管理路由稳定返回 503。
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

fn require_memory_services() -> Result<Arc<MemoryServices>, (StatusCode, Json<ErrorResponse>)> {
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
    let request = MemoryRetrievalRequest::bind(params, scope).map_err(memory_error_response)?;
    let receipt = services
        .retriever
        .retrieve(&request)
        .map_err(memory_error_response)?;
    // category/importance 为管理页页内过滤，不改变检索排序与游标语义。
    let items = receipt
        .items
        .into_iter()
        .filter(|item| {
            query
                .category
                .is_none_or(|category| item.category == category)
                && query
                    .importance
                    .is_none_or(|importance| item.importance == importance)
        })
        .collect();
    Ok(Json(MemoryQueryPageReceipt {
        items,
        has_more: receipt.has_more,
        next_cursor: receipt.next_cursor,
    }))
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
