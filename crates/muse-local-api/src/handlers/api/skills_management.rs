// 用户 Skill 管理 API，只操作 Muse 数据目录下的 `skills/`。

use muse_core::domain::skill::{
    SkillDraft, SkillRecord, SkillStore, SkillStoreError, SkillStoreErrorKind, SkillSummary,
};

fn user_skill_store(state: &AppState) -> SkillStore {
    SkillStore::from_data_dir(state.runtime_service.data_dir())
}

fn skill_store_error_response(
    error: SkillStoreError,
) -> (StatusCode, Json<ErrorResponse>) {
    let (status, prefix) = match error.kind {
        SkillStoreErrorKind::Invalid => (StatusCode::BAD_REQUEST, "skill_invalid："),
        SkillStoreErrorKind::NotFound => (StatusCode::NOT_FOUND, ""),
        SkillStoreErrorKind::Conflict => (StatusCode::CONFLICT, "skill_conflict："),
        SkillStoreErrorKind::RevisionConflict => {
            (StatusCode::CONFLICT, "skill_revision_conflict：")
        }
        SkillStoreErrorKind::Io => (StatusCode::INTERNAL_SERVER_ERROR, ""),
    };
    (
        status,
        Json(ErrorResponse {
            error: format!("{prefix}{}", error.message),
        }),
    )
}

pub(crate) async fn handle_skills(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SkillSummary>>, (StatusCode, Json<ErrorResponse>)> {
    let mut config = state.user_config.lock().await;
    config
        .refresh_from_disk()
        .map_err(SkillStoreError::from_config)
        .map_err(skill_store_error_response)?;
    user_skill_store(&state)
        .list(config.skill_preferences())
        .map(Json)
        .map_err(skill_store_error_response)
}

pub(crate) async fn handle_create_skill(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SkillCreateRequest>,
) -> Result<(StatusCode, Json<SkillRecord>), (StatusCode, Json<ErrorResponse>)> {
    let mut config = state.user_config.lock().await;
    let record = user_skill_store(&state)
        .create(SkillDraft {
            name: request.name,
            description: request.description,
            content: request.content,
            enabled: request.enabled,
        }, &mut config)
        .map_err(skill_store_error_response)?;
    state.runtime_service.touch();
    Ok((StatusCode::CREATED, Json(record)))
}

pub(crate) async fn handle_get_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SkillRecord>, (StatusCode, Json<ErrorResponse>)> {
    let mut config = state.user_config.lock().await;
    config
        .refresh_from_disk()
        .map_err(SkillStoreError::from_config)
        .map_err(skill_store_error_response)?;
    user_skill_store(&state)
        .get(&name, config.skill_preferences())
        .map(Json)
        .map_err(skill_store_error_response)
}

pub(crate) async fn handle_update_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(request): Json<SkillUpdateRequest>,
) -> Result<Json<SkillRecord>, (StatusCode, Json<ErrorResponse>)> {
    let mut config = state.user_config.lock().await;
    let record = user_skill_store(&state)
        .update(
            &name,
            &request.revision,
            SkillDraft {
                name: request.name,
                description: request.description,
                content: request.content,
                enabled: request.enabled,
            },
            &mut config,
        )
        .map_err(skill_store_error_response)?;
    state.runtime_service.touch();
    Ok(Json(record))
}

pub(crate) async fn handle_delete_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut config = state.user_config.lock().await;
    user_skill_store(&state)
        .delete(&name, &query.revision, &mut config)
        .map_err(skill_store_error_response)?;
    state.runtime_service.touch();
    Ok(StatusCode::NO_CONTENT)
}
