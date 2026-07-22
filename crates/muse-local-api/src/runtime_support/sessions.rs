//! 运行时会话 HTTP 适配与会话级读取辅助。

use super::*;

/// 查询运行时会话列表。
pub(crate) async fn handle_runtime_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RuntimeSessionListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let payload = runtime_session_list_payload(&state)
        .await
        .map_err(internal_error)?;
    let active_conversation_id = active_conversation_id(&state);
    Ok(Json(RuntimeSessionListResponse {
        active_conversation_id: active_conversation_id.clone(),
        status: if payload.exists {
            "当前存在 runtime transcript。".to_string()
        } else {
            "当前尚未写入 runtime transcript。".to_string()
        },
        sessions: runtime_session_list_for_active(payload.sessions, &active_conversation_id),
    }))
}

/// 将会话元数据作为完整快照事件追加到 Session v3，并同步可重建索引。
pub(crate) async fn handle_runtime_session_metadata_patch(
    State(state): State<Arc<AppState>>,
    Path(conversation_id): Path<String>,
    Json(patch): Json<RuntimeSessionMetadataPatch>,
) -> Result<Json<RuntimeSessionMetadataResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_runtime_session(&state, &conversation_id).await?;
    if patch.title.is_none() && patch.archived.is_none() {
        return Err(bad_request("至少需要提交 title 或 archived。"));
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let metadata = repository
        .update_metadata(&conversation_id, patch.title, patch.archived)
        .await
        .map_err(|error| match error {
            muse_runtime::session_metadata::SessionRepositoryError::InvalidInput(message) => {
                bad_request(&message)
            }
            muse_runtime::session_metadata::SessionRepositoryError::InvalidData(message) => {
                (StatusCode::CONFLICT, Json(ErrorResponse { error: message }))
            }
            other => internal_error(other.to_string()),
        })?;
    let persona_status = {
        let personas = state.personas.lock().await;
        if personas.get(&metadata.persona_id).is_some() {
            "bound"
        } else {
            "missing"
        }
    };
    Ok(Json(RuntimeSessionMetadataResponse {
        conversation_id,
        persona_id: metadata.persona_id,
        persona_name_snapshot: metadata.persona_name_snapshot,
        persona_version_snapshot: metadata.persona_version_snapshot,
        persona_status: persona_status.to_string(),
        title: metadata.title,
        archived: metadata.archived,
        source_conversation_id: metadata.source_conversation_id,
        updated_at: metadata.updated_at,
        revision: metadata.revision,
    }))
}

/// 导出可公开 transcript，不包含秘密、内部路径和工具原始外置结果。
pub(crate) async fn handle_runtime_session_export(
    State(state): State<Arc<AppState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<RuntimeSessionExportResponse>, (StatusCode, Json<ErrorResponse>)> {
    let events = runtime_session_events(&state, &conversation_id).await?;
    let (conversation, _) = replay_runtime_transcript_lines(
        "Muse 会话导出".to_string(),
        state.config.agent.max_history,
        &session_events_as_jsonl(&events),
        &conversation_id,
        None,
    );
    let messages = conversation
        .messages
        .iter()
        .filter_map(|message| match message.role {
            Role::User => Some(RuntimeSessionExportMessage {
                role: "user".to_string(),
                content: message.content.clone(),
            }),
            Role::Assistant => Some(RuntimeSessionExportMessage {
                role: "assistant".to_string(),
                content: message.content.clone(),
            }),
            Role::System | Role::Tool => None,
        })
        .collect();
    let item = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?
        .metadata(&conversation_id)
        .await
        .map_err(session_metadata_read_error)?;
    let metadata =
        item.ok_or_else(|| internal_error("会话缺少 v2 Persona metadata。".to_string()))?;
    let persona_exists = {
        let personas = state.personas.lock().await;
        personas.get(&metadata.persona_id).is_some()
    };
    Ok(Json(RuntimeSessionExportResponse {
        schema_version: "muse-session-export/v1".to_string(),
        conversation_id,
        persona_id: metadata.persona_id,
        persona_name_snapshot: metadata.persona_name_snapshot,
        persona_version_snapshot: metadata.persona_version_snapshot,
        persona_status: if persona_exists { "bound" } else { "missing" }.to_string(),
        title: metadata.title,
        archived: metadata.archived,
        source_conversation_id: metadata.source_conversation_id,
        exported_at: chrono::Utc::now().to_rfc3339(),
        messages,
    }))
}

/// 返回 Context Inspector 所需的上下文与冻结运行策略。
pub(crate) async fn handle_runtime_session_context(
    State(state): State<Arc<AppState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<RuntimeSessionContextResponse>, (StatusCode, Json<ErrorResponse>)> {
    let events = runtime_session_events(&state, &conversation_id).await?;
    let runtime_policy_snapshot = events
        .iter()
        .rev()
        .find(|event| event.kind == "runtime_policy_snapshot")
        .and_then(|event| event.payload.get("snapshot"))
        .cloned();
    let usage = RuntimeUsageStore::load_from_dir(state.runtime_service.data_dir())
        .map_err(|error| internal_error(error.to_string()))?;
    let context_snapshot = usage
        .latest_context_snapshot(&conversation_id)
        .map_err(|error| internal_error(error.to_string()))?;
    Ok(Json(RuntimeSessionContextResponse {
        conversation_id,
        context_snapshot,
        runtime_policy_snapshot,
        status: "会话上下文与运行策略已读取。".to_string(),
    }))
}

/// 兼容单独读取冻结运行策略的只读接口。
pub(crate) async fn handle_runtime_session_runtime_profile(
    State(state): State<Arc<AppState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let events = runtime_session_events(&state, &conversation_id).await?;
    let snapshot = events
        .iter()
        .rev()
        .find(|event| event.kind == "runtime_policy_snapshot")
        .and_then(|event| event.payload.get("snapshot"))
        .cloned();
    Ok(Json(serde_json::json!({
        "conversation_id": conversation_id,
        "snapshot": snapshot,
        "status": "冻结运行策略已读取。"
    })))
}

async fn runtime_session_events(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<Vec<muse_runtime::session::SessionEventV3>, (StatusCode, Json<ErrorResponse>)> {
    if conversation_id.trim().is_empty() {
        return Err(bad_request("会话 ID 不能为空。"));
    }
    let store = state
        .runtime_service
        .session_store()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let events = store
        .events_for_conversation(conversation_id)
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    if events.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "session_not_found：会话不存在。".to_string(),
            }),
        ));
    }
    Ok(events)
}

async fn require_runtime_session(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    runtime_session_events(state, conversation_id)
        .await
        .map(|_| ())
}

pub(super) fn session_metadata_read_error(
    error: muse_runtime::session_metadata::SessionRepositoryError,
) -> (StatusCode, Json<ErrorResponse>) {
    match error {
        muse_runtime::session_metadata::SessionRepositoryError::InvalidInput(message) => {
            bad_request(&message)
        }
        muse_runtime::session_metadata::SessionRepositoryError::InvalidData(message) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: if message.starts_with("session_persona_unbound") {
                    message
                } else {
                    format!("session_persona_unbound：{message}")
                },
            }),
        ),
        other => internal_error(other.to_string()),
    }
}

/// 恢复指定运行时会话。
pub(crate) async fn handle_runtime_session_resume(
    State(state): State<Arc<AppState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<RuntimeSessionResumeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "resume_session")?;
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let metadata = repository
        .metadata(&conversation_id)
        .await
        .map_err(session_metadata_read_error)?
        .ok_or_else(|| bad_request("会话缺少 v2 Persona metadata。"))?;
    let (persona, previous_personas) = {
        let mut personas = state.personas.lock().await;
        let previous = personas.clone();
        let persona = personas.get(&metadata.persona_id).cloned().ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "session_persona_missing：会话所属角色已删除，只能查看或导出。"
                        .to_string(),
                }),
            )
        })?;
        personas
            .set_active(&persona.id)
            .map_err(persona_store_error_response)?;
        personas.save().map_err(persona_store_error_response)?;
        (persona, previous)
    };
    let previous_conversation = state.runtime_service.lock_conversation().await.clone();
    let previous_todos = state.runtime_service.runtime_todos().await;
    let previous_conversation_id = active_conversation_id(&state);
    let runtime = ConversationRuntime::new(state.clone(), false);
    let outcome = match runtime
        .submit(
            RuntimeOp::ResumeSession {
                conversation_id: conversation_id.clone(),
            },
            RuntimeEventEmitter::collect_only(),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            rollback_persona_runtime_transition(
                &state,
                previous_personas,
                previous_conversation,
                previous_todos,
                previous_conversation_id,
            )
            .await
            .map_err(internal_error)?;
            let _ = finish_runtime_idle_lease(idle_lease);
            return Err(bad_request(&error));
        }
    };
    let restored_messages = {
        let conv = state.runtime_service.lock_conversation().await;
        conv.messages
            .iter()
            .filter(|message| message.role != Role::System)
            .count()
    };
    if let Err(error) = repository.set_workspace_state(&persona.id, &conversation_id) {
        rollback_persona_runtime_transition(
            &state,
            previous_personas,
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
            previous_personas,
            previous_conversation,
            previous_todos,
            previous_conversation_id,
        )
        .await
        .map_err(internal_error)?;
        return Err(internal_error(error));
    }
    finish_runtime_idle_lease(idle_lease)?;
    state.runtime_service.touch();

    Ok(Json(RuntimeSessionResumeResponse {
        conversation_id,
        persona_id: metadata.persona_id,
        persona_name_snapshot: metadata.persona_name_snapshot,
        persona_version_snapshot: metadata.persona_version_snapshot,
        persona_status: "bound".to_string(),
        restored_messages,
        status: outcome.reply,
    }))
}

/// 从指定运行时会话分叉出新会话。
pub(crate) async fn handle_runtime_session_fork(
    State(state): State<Arc<AppState>>,
    Path(source_conversation_id): Path<String>,
    Json(req): Json<RuntimeSessionForkRequest>,
) -> Result<Json<RuntimeSessionForkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    let idle_lease = acquire_runtime_idle_lease(&state, "fork_session")?;
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let source_metadata = repository
        .metadata(&source_conversation_id)
        .await
        .map_err(session_metadata_read_error)?
        .ok_or_else(|| bad_request("来源会话缺少 v2 Persona metadata。"))?;
    let source_approval_mode = repository
        .approval_mode_for_resume(&source_conversation_id)
        .await
        .map_err(session_metadata_read_error)?;
    let target_persona_id = req
        .target_persona_id
        .clone()
        .unwrap_or_else(|| source_metadata.persona_id.clone());
    let (target_persona, previous_personas) = {
        let mut personas = state.personas.lock().await;
        let previous = personas.clone();
        let persona = personas.get(&target_persona_id).cloned().ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("目标角色 `{target_persona_id}` 不存在。"),
                }),
            )
        })?;
        personas
            .set_active(&target_persona_id)
            .map_err(persona_store_error_response)?;
        personas.save().map_err(persona_store_error_response)?;
        (persona, previous)
    };
    let previous_conversation = state.runtime_service.lock_conversation().await.clone();
    let previous_todos = state.runtime_service.runtime_todos().await;
    let previous_conversation_id = active_conversation_id(&state);
    let runtime = ConversationRuntime::new(state.clone(), false);
    let outcome = match runtime
        .submit(
            RuntimeOp::ForkSession {
                source_conversation_id: source_conversation_id.clone(),
                before_user_message_index: req.before_user_message_index,
            },
            RuntimeEventEmitter::collect_only(),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            rollback_persona_runtime_transition(
                &state,
                previous_personas,
                previous_conversation,
                previous_todos,
                previous_conversation_id,
            )
            .await
            .map_err(internal_error)?;
            let _ = finish_runtime_idle_lease(idle_lease);
            return Err(bad_request(&error));
        }
    };
    let conversation_id = active_conversation_id(&state);
    let restored_messages = {
        let conv = state.runtime_service.lock_conversation().await;
        conv.messages
            .iter()
            .filter(|message| message.role != Role::System)
            .count()
    };
    let metadata = repository
        .metadata(&conversation_id)
        .await
        .map_err(session_metadata_read_error)?
        .ok_or_else(|| internal_error("分叉会话缺少 v2 Persona metadata。".to_string()))?;
    if let Err(error) = repository.set_workspace_state(&target_persona.id, &conversation_id) {
        rollback_persona_runtime_transition(
            &state,
            previous_personas,
            previous_conversation,
            previous_todos,
            previous_conversation_id,
        )
        .await
        .map_err(internal_error)?;
        return Err(internal_error(error.to_string()));
    }
    let inherited_approval_mode = repository
        .update_approval_mode(&conversation_id, source_approval_mode.preset)
        .await
        .map_err(session_metadata_read_error)?;
    set_active_approval_mode(
        &state,
        inherited_approval_mode.preset,
        inherited_approval_mode.revision,
    )
    .map_err(internal_error)?;
    finish_runtime_idle_lease(idle_lease)?;
    state.runtime_service.touch();

    Ok(Json(RuntimeSessionForkResponse {
        conversation_id,
        persona_id: metadata.persona_id,
        persona_name_snapshot: metadata.persona_name_snapshot,
        persona_version_snapshot: metadata.persona_version_snapshot,
        persona_status: "bound".to_string(),
        source_conversation_id,
        before_user_message_index: req.before_user_message_index,
        restored_messages,
        status: outcome.reply,
    }))
}

/// 删除指定运行时会话。
pub(crate) async fn handle_runtime_session_delete(
    State(state): State<Arc<AppState>>,
    Path(conversation_id): Path<String>,
) -> Result<Json<RuntimeSessionDeleteResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    require_active_persona_http(&state).await?;
    if conversation_id.trim().is_empty() {
        return Err(bad_request("会话 ID 不能为空。"));
    }
    let idle_lease = acquire_runtime_idle_lease(&state, "delete_session")?;
    let delete_result = delete_runtime_transcript_for_conversation(&state, &conversation_id)
        .await
        .map_err(|err| internal_error(format!("删除会话 transcript 失败：{err}")))?;
    if delete_result.deleted_records == 0 && delete_result.deleted_files == 0 {
        return Err(bad_request("会话不存在或没有可删除的 transcript。"));
    }
    let mut active_after_delete = active_conversation_id(&state);
    if active_after_delete == conversation_id {
        let system_prompt = current_runtime_system_prompt(&state).await;
        let mut conv = state.runtime_service.lock_conversation().await;
        *conv = muse_core::domain::conversation::Conversation::new(
            system_prompt,
            state.config.agent.max_history,
        );
        drop(conv);
        replace_runtime_todos(&state, Vec::new()).await;
        active_after_delete = next_runtime_session_id();
        set_active_conversation_id(&state, &active_after_delete).map_err(internal_error)?;
        initialize_new_session_approval_mode(&state, &active_after_delete)
            .await
            .map_err(internal_error)?;
    }
    finish_runtime_idle_lease(idle_lease)?;
    state.runtime_service.touch();

    Ok(Json(RuntimeSessionDeleteResponse {
        conversation_id,
        active_conversation_id: active_after_delete,
        deleted_records: delete_result.deleted_records,
        deleted_files: delete_result.deleted_files,
        status: "会话已删除。".to_string(),
    }))
}
