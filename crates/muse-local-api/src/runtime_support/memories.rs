//! Persona 长期记忆管理 API 的适配实现。
//!
//! 所有读取经 `MemoryRetriever` 与 `MemoryRepository::current` 端口，内容写入经
//! `apply_management_content_mutation`（敏感双门），重要程度经 `adjust_importance`
//! （不创建内容 revision），删除经 `delete_confirmed`（先删除权威后主库）。
//! 本工作树未接线真实实例，管理路由一律返回 503 与对应 `memory_*` 稳定码；
//! 真实实例由协调者在临时集成分支通过 `install_memory_services` 注入。

use muse_core::app::memory_storage::SqliteMemoryRepository;
use muse_core::domain::memory::{
    ConfirmedMemoryDeleteRequest, MemoryCursor, MemoryDeleteConfirmation,
    MemoryDeleteConfirmationSource, MemoryDeleteParams, MemoryDeleteReceipt, MemoryError,
    MemoryErrorCode, MemoryId, MemoryImportanceAdjustment, MemoryImportanceAdjustmentReceipt,
    MemoryManagementAuthorization, MemoryManagementBinding, MemoryManagementContentMutation,
    MemoryManagementContentParams, MemoryMutationReceipt, MemoryPersonaScope,
    MemoryQueryPageReceipt, MemoryQueryParams, MemoryRepository, MemoryRetrievalRequest,
    MemoryRetriever, MemoryRevision, MemoryRevisionId, MemorySensitivityPolicy,
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

/// 记忆管理 API 的运行时服务接缝；真实实例由协调者在集成分支启动流程注入。
///
/// Repository 使用具体的 SQLite 实现：`delete_confirmed` 要求删除权威与
/// Repository 内部实例同一指针，因此删除调用一律携带 `repository.deletion_authority()`，
/// 不单独持有权威句柄，避免跨实例被判为不可用。
pub struct MemoryServices {
    pub repository: Arc<SqliteMemoryRepository>,
    pub retriever: Arc<dyn MemoryRetriever>,
    pub sensitivity: Arc<dyn MemorySensitivityPolicy>,
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

fn management_binding(
    scope: MemoryPersonaScope,
    operation_id: Option<String>,
) -> Result<MemoryManagementBinding, (StatusCode, Json<ErrorResponse>)> {
    let now = chrono::Utc::now().to_rfc3339();
    let authorization = MemoryManagementAuthorization::from_runtime(
        scope,
        next_runtime_id("memory-action"),
        now.clone(),
    )
    .map_err(memory_error_response)?;
    MemoryManagementBinding::bind(
        authorization,
        operation_id.unwrap_or_else(|| next_runtime_id("memory-op")),
        now.clone(),
        now.clone(),
        now,
    )
    .map_err(memory_error_response)
}

fn apply_management_content(
    services: &MemoryServices,
    params: MemoryManagementContentParams,
    binding: MemoryManagementBinding,
    assigned_memory_id: MemoryId,
) -> Result<MemoryMutationReceipt, (StatusCode, Json<ErrorResponse>)> {
    let mutation = MemoryManagementContentMutation::bind(
        params,
        binding,
        assigned_memory_id,
        MemoryRevisionId(next_runtime_id("memory-rev")),
        services.sensitivity.as_ref(),
    )
    .map_err(memory_error_response)?;
    // Repository 在事务内重跑 RepositoryCommit 敏感门，双门不旁路。
    services
        .repository
        .apply_management_content_mutation(&mutation, services.sensitivity.as_ref())
        .map_err(memory_error_response)
}

fn confirmed_delete_request(
    scope: MemoryPersonaScope,
    params: MemoryDeleteParams,
) -> Result<ConfirmedMemoryDeleteRequest, MemoryError> {
    let confirmation = MemoryDeleteConfirmation::new(
        next_runtime_id("memory-confirm"),
        chrono::Utc::now().to_rfc3339(),
        MemoryDeleteConfirmationSource::PersonaManagement {
            action_id: next_runtime_id("memory-action"),
        },
    )?;
    ConfirmedMemoryDeleteRequest::bind(params, scope, confirmation)
}

/// 清空指定 Persona 的全部记忆；handle_delete_persona 与全清路由共用同一删除权威链路。
pub(super) fn delete_all_persona_memories(
    services: &MemoryServices,
    persona_id: &str,
) -> Result<MemoryDeleteReceipt, MemoryError> {
    let scope = MemoryPersonaScope::new(persona_id)?;
    let request = confirmed_delete_request(scope, MemoryDeleteParams::PersonaAll)?;
    services
        .repository
        .delete_confirmed(&request, services.repository.deletion_authority())
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
    Query(query): Query<MemoryListQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MemoryQueryPageReceipt>, (StatusCode, Json<ErrorResponse>)> {
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
    Json(request): Json<MemoryCreateRequest>,
) -> Result<(StatusCode, Json<MemoryMutationReceipt>), (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let binding = management_binding(scope, request.operation_id)?;
    let params = MemoryManagementContentParams::Create {
        category: request.category,
        content: request.content,
        importance: request.importance,
        event_time: request.event_time,
        change_reason: request.change_reason,
    };
    let receipt = apply_management_content(
        &services,
        params,
        binding,
        MemoryId(next_runtime_id("memory")),
    )?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

/// 纠正记忆；expected_revision_id 并发校验失败返回 409 memory_revision_conflict。
pub(crate) async fn handle_correct_persona_memory(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<MemoryCorrectRequest>,
) -> Result<Json<MemoryMutationReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let memory_id = MemoryId(memory_id);
    let binding = management_binding(scope, request.operation_id)?;
    let params = MemoryManagementContentParams::Correct {
        memory_id: memory_id.clone(),
        expected_revision_id: request.expected_revision_id,
        category: request.category,
        content: request.content,
        event_time: request.event_time,
        change_reason: request.change_reason,
    };
    let receipt = apply_management_content(&services, params, binding, memory_id)?;
    Ok(Json(receipt))
}

/// 调整重要程度；只更新逻辑 entry，不创建内容 revision。
pub(crate) async fn handle_adjust_persona_memory_importance(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<MemoryImportanceAdjustRequest>,
) -> Result<Json<MemoryImportanceAdjustmentReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let binding = management_binding(scope, request.operation_id)?;
    let adjustment = MemoryImportanceAdjustment::bind(
        binding,
        MemoryId(memory_id),
        request.expected_revision_id,
        request.expected_importance,
        request.importance,
    )
    .map_err(memory_error_response)?;
    let receipt = services
        .repository
        .adjust_importance(&adjustment)
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

/// 删除单条记忆；确认证据由服务端以 Persona 管理来源构造。
pub(crate) async fn handle_delete_persona_memory(
    Path((persona_id, memory_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MemoryDeleteReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let request = confirmed_delete_request(
        scope,
        MemoryDeleteParams::Memory {
            memory_id: MemoryId(memory_id),
        },
    )
    .map_err(memory_error_response)?;
    let receipt = services
        .repository
        .delete_confirmed(&request, services.repository.deletion_authority())
        .map_err(memory_error_response)?;
    Ok(Json(receipt))
}

/// 清空该 Persona 的全部记忆。
pub(crate) async fn handle_clear_persona_memories(
    Path(persona_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<MemoryDeleteReceipt>, (StatusCode, Json<ErrorResponse>)> {
    let scope = require_existing_persona_scope(&state, &persona_id).await?;
    let services = require_memory_services()?;
    let request = confirmed_delete_request(scope, MemoryDeleteParams::PersonaAll)
        .map_err(memory_error_response)?;
    let receipt = services
        .repository
        .delete_confirmed(&request, services.repository.deletion_authority())
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
