/// 返回删除角色会影响的历史会话和工作区状态。
pub(crate) async fn handle_persona_deletion_impact(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<PersonaDeletionImpactResponse>, (StatusCode, Json<ErrorResponse>)> {
    {
        let personas = state.personas.lock().await;
        if personas.get(&id).is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("角色 `{id}` 不存在"),
                }),
            ));
        }
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    Ok(Json(PersonaDeletionImpactResponse {
        persona_id: id.clone(),
        associated_session_count: repository
            .persona_session_count(&id)
            .await
            .map_err(|error| internal_error(error.to_string()))?,
        workspace_state_exists: repository
            .workspace_state_exists(&id)
            .map_err(|error| internal_error(error.to_string()))?,
    }))
}

/// 激活指定角色，并恢复该角色上次使用的会话。
pub(crate) async fn handle_activate_persona(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<PersonaMutationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "activate_persona")?;
    let (active_persona, previous_store) = {
        let personas = state.personas.lock().await;
        let previous_store = personas.clone();
        let active = personas.get(&id).cloned().ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("角色 `{id}` 不存在"),
                }),
            )
        })?;
        (active, previous_store)
    };

    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let latest = repository
        .preferred_conversation_for_persona(&active_persona.id)
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let previous_conversation = state.runtime_service.lock_conversation().await.clone();
    let previous_todos = state.runtime_service.runtime_todos().await;
    let previous_conversation_id = active_conversation_id(&state);
    {
        let mut personas = state.personas.lock().await;
        personas
            .set_active(&id)
            .map_err(persona_store_error_response)?;
        personas.save().map_err(persona_store_error_response)?;
    }
    let session_restored = if let Some(conversation_id) = latest {
        let runtime = ConversationRuntime::new(state.clone(), false);
        if let Err(error) = runtime
            .submit(
                RuntimeOp::ResumeSession {
                    conversation_id: conversation_id.clone(),
                },
                RuntimeEventEmitter::collect_only(),
            )
            .await
        {
            rollback_persona_runtime_transition(
                &state,
                previous_store,
                previous_conversation,
                previous_todos,
                previous_conversation_id,
            )
            .await
            .map_err(internal_error)?;
            return Err(internal_error(format!("恢复角色最近会话失败：{error}")));
        }
        if let Err(error) = repository.set_workspace_state(&active_persona.id, &conversation_id) {
            rollback_persona_runtime_transition(
                &state,
                previous_store,
                previous_conversation,
                previous_todos,
                previous_conversation_id,
            )
            .await
            .map_err(internal_error)?;
            return Err(internal_error(error.to_string()));
        }
        if let Err(error) = restore_active_approval_mode(&state, &conversation_id).await {
            rollback_persona_runtime_transition(
                &state,
                previous_store,
                previous_conversation,
                previous_todos,
                previous_conversation_id,
            )
            .await
            .map_err(internal_error)?;
            return Err(internal_error(format!(
                "恢复会话审批模式失败：{error}"
            )));
        }
        true
    } else {
        reset_conversation_for_active_persona(&state).await;
        false
    };

    finish_runtime_idle_lease(idle_lease)?;
    let mut response = persona_mutation_response(&state, active_persona, true).await;
    response.session_restored = session_restored;
    Ok(Json(response))
}

async fn rollback_persona_runtime_transition(
    state: &Arc<AppState>,
    previous_store: PersonaStore,
    previous_conversation: Conversation,
    previous_todos: Vec<RuntimeTodoItem>,
    previous_conversation_id: String,
) -> Result<(), String> {
    {
        let mut personas = state.personas.lock().await;
        *personas = previous_store;
        personas.save().map_err(|error| error.to_string())?;
    }
    {
        let mut conversation = state.runtime_service.lock_conversation().await;
        *conversation = previous_conversation;
    }
    state
        .runtime_service
        .replace_runtime_todos(previous_todos)
        .await;
    set_active_conversation_id(state, &previous_conversation_id)?;
    Ok(())
}

/// 在本地 API 开始接收请求前恢复活动角色的工作区会话。
pub(crate) async fn initialize_active_persona_session(state: &Arc<AppState>) -> Result<(), String> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = state
        .runtime_service
        .acquire_idle_lease("startup_persona_session_restore")
        .map_err(|error| format!("启动恢复无法取得空闲租约：{error}"))?;
    let active_persona = {
        let personas = state.personas.lock().await;
        personas.active_persona().cloned()
    };
    let Some(active_persona) = active_persona else {
        idle_lease
            .finish()
            .map_err(|error| format!("启动恢复释放空闲租约失败：{error}"))?;
        return Ok(());
    };
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| error.to_string())?;
    let preferred = repository
        .preferred_conversation_for_persona(&active_persona.id)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(conversation_id) = preferred {
        ConversationRuntime::new(state.clone(), false)
            .submit(
                RuntimeOp::ResumeSession {
                    conversation_id: conversation_id.clone(),
                },
                RuntimeEventEmitter::collect_only(),
            )
            .await
            .map_err(|error| format!("启动恢复角色会话失败：{error}"))?;
        repository
            .set_workspace_state(&active_persona.id, &conversation_id)
            .map_err(|error| error.to_string())?;
        restore_active_approval_mode(state, &conversation_id).await?;
    } else {
        reset_conversation_for_active_persona(state).await;
    }
    idle_lease
        .finish()
        .map_err(|error| format!("启动恢复释放空闲租约失败：{error}"))?;
    Ok(())
}
