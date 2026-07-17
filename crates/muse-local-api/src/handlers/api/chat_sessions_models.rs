/// 处理非流式聊天请求。
pub(crate) async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, Json<ErrorResponse>)> {
    let selected_skill = normalize_selected_skill(req.selected_skill)?;
    let runtime = ConversationRuntime::new(state.clone(), false);
    let prepared = {
        let _transition = state.persona_runtime_transition_gate.lock().await;
        let active_persona = require_active_persona_http(&state).await?;
        runtime
            .occupy_user_turn(active_persona)
            .map_err(runtime_turn_prepare_error_response)?
    };
    Ok(match runtime
        .run_prepared_user_turn(
            req.message,
            selected_skill,
            RuntimeEventEmitter::collect_only(),
            prepared,
        )
        .await
    {
        Ok(outcome) => Json(ChatResponse {
            reply: outcome.reply,
        }),
        Err(err) => Json(ChatResponse { reply: err }),
    })
}

/// 处理流式聊天请求。
pub(crate) async fn handle_chat_stream(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatStreamRequest>,
) -> Result<
    Sse<impl Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>,
    (StatusCode, Json<ErrorResponse>),
> {
    let selected_skill = normalize_selected_skill(request.selected_skill.clone())?;
    let client_request_id = request.client_request_id.trim();
    if client_request_id.is_empty()
        || client_request_id.len() > 128
        || !client_request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(bad_request(
            "client_request_id 必须是 1 到 128 位字母、数字、连字符或下划线。",
        ));
    }
    let client_request_id = client_request_id.to_string();
    let runtime = ConversationRuntime::new(state.clone(), request.voice_enabled.unwrap_or(false));
    let prepared = {
        // 角色事实、会话 ID、turn 占位与幂等登记必须在同一 transition gate 内提交。
        let _transition = state.persona_runtime_transition_gate.lock().await;
        let active_persona = require_active_persona_http(&state).await?;
        let conversation_id = request.conversation_id.trim();
        if conversation_id.is_empty() || conversation_id != active_conversation_id(&state) {
            return Err(decision_conflict(
                "聊天请求的 conversation_id 已过期，请先同步运行时状态。".to_string(),
            ));
        }
        if state
            .chat_request_ids
            .lock()
            .await
            .contains(&client_request_id)
        {
            return Err(decision_conflict(format!(
                "聊天请求 `{client_request_id}` 已受理，不会重复创建回合。"
            )));
        }
        let prepared = runtime
            .occupy_user_turn(active_persona)
            .map_err(runtime_turn_prepare_error_response)?;
        let mut request_ids = state.chat_request_ids.lock().await;
        match request_ids.accept(&client_request_id) {
            Ok(()) => {}
            Err(ChatRequestRegistryError::Duplicate) => {
                return Err(decision_conflict(format!(
                    "聊天请求 `{client_request_id}` 已受理，不会重复创建回合。"
                )));
            }
            Err(ChatRequestRegistryError::CapacityExceeded) => {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(ErrorResponse {
                        error: "聊天请求幂等登记表已满，请等待已有请求超过保护期限后重试。"
                            .to_string(),
                    }),
                ));
            }
            Err(ChatRequestRegistryError::Persistence(error)) => {
                return Err(internal_error(error));
            }
        }
        drop(request_ids);
        prepared
    };
    let (tx, rx) =
        mpsc::channel::<Result<axum::response::sse::Event, std::convert::Infallible>>(64);
    let request_state = state.clone();
    let message = request.message;
    let emitter = RuntimeEventEmitter::stream(tx.clone());
    tokio::spawn(async move {
        if !emitter.is_closed() {
            let _ = runtime
                .run_prepared_user_turn(message, selected_skill, emitter.clone(), prepared)
                .await;
        }
        if let Err(error) = request_state
            .chat_request_ids
            .lock()
            .await
            .mark_terminal(&client_request_id)
        {
            tracing::error!(
                target: "muse::runtime",
                error = %error,
                "持久化聊天请求终态失败"
            );
        }
    });

    Ok(
        Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx)).keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        ),
    )
}

fn normalize_selected_skill(
    selected_skill: Option<String>,
) -> Result<Option<String>, (StatusCode, Json<ErrorResponse>)> {
    let Some(skill_name) = selected_skill else {
        return Ok(None);
    };
    let skill_name = skill_name.trim().to_string();
    muse_core::domain::skill::validate_skill_name(&skill_name)
        .map_err(|error| bad_request(&error.to_string()))?;
    Ok(Some(skill_name))
}

/// 明确拒绝旧版 GET 流式聊天，避免查询字符串触发有副作用的用户轮次。
pub(crate) async fn handle_chat_stream_get_not_allowed() -> Response {
    let mut response = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(ErrorResponse {
            error: "流式聊天只接受 POST JSON 请求。".to_string(),
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

/// 返回经过鉴权的本地运行时健康状态。
pub(crate) async fn handle_runtime_health(
    Extension(security): Extension<Arc<LocalApiSecurity>>,
) -> Json<RuntimeHealthResponse> {
    Json(security.health())
}

/// 为浏览器 WebSocket 握手签发短期单次票据。
pub(crate) async fn handle_runtime_ws_ticket(
    Extension(security): Extension<Arc<LocalApiSecurity>>,
) -> Response {
    match security.issue_ws_ticket().await {
        Ok(ticket) => Json::<WsTicketResponse>(ticket).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    pub(crate) conversation_id: Option<String>,
}

/// 查询当前会话历史。
pub(crate) async fn handle_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> Json<HistoryResponse> {
    if let Some(ref cid) = query.conversation_id {
        let active_cid = active_conversation_id(&state);
        if cid != &active_cid {
            if let Ok(content) = read_runtime_transcript_for_conversation(&state, cid).await {
                let system_prompt = current_runtime_system_prompt(&state).await;
                let (conv, _stats) = replay_runtime_transcript_lines(
                    system_prompt,
                    state.config.agent.max_history,
                    &content,
                    cid,
                    None,
                );
                return Json(HistoryResponse {
                    messages: conv
                        .api_messages()
                        .iter()
                        .filter(|message| {
                            message.role != Role::System
                                && message.role != Role::Tool
                                && message.tool_call_id.is_none()
                        })
                        .cloned()
                        .collect(),
                });
            } else {
                return Json(HistoryResponse {
                    messages: Vec::new(),
                });
            }
        }
    }

    let conv = state.runtime_service.lock_conversation().await;
    Json(HistoryResponse {
        messages: conv
            .api_messages()
            .iter()
            .filter(|message| {
                message.role != Role::System
                    && message.role != Role::Tool
                    && message.tool_call_id.is_none()
            })
            .cloned()
            .collect(),
    })
}

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
            muse_runtime::session_metadata::SessionRepositoryError::InvalidData(message) => (
                StatusCode::CONFLICT,
                Json(ErrorResponse { error: message }),
            ),
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
    let metadata = item.ok_or_else(|| internal_error("会话缺少 v2 Persona metadata。".to_string()))?;
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
    runtime_session_events(state, conversation_id).await.map(|_| ())
}

fn session_metadata_read_error(
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
        let persona = personas.get(&metadata.persona_id).cloned().ok_or_else(|| (
            StatusCode::CONFLICT,
            Json(ErrorResponse { error: "session_persona_missing：会话所属角色已删除，只能查看或导出。".to_string() }),
        ))?;
        personas.set_active(&persona.id).map_err(persona_store_error_response)?;
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
    let target_persona_id = req
        .target_persona_id
        .clone()
        .unwrap_or_else(|| source_metadata.persona_id.clone());
    let (target_persona, previous_personas) = {
        let mut personas = state.personas.lock().await;
        let previous = personas.clone();
        let persona = personas.get(&target_persona_id).cloned().ok_or_else(|| (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse { error: format!("目标角色 `{target_persona_id}` 不存在。") }),
        ))?;
        personas.set_active(&target_persona_id).map_err(persona_store_error_response)?;
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

/// 重置当前会话。
pub(crate) async fn handle_reset(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    require_active_persona_http(&state).await?;
    let idle_lease = acquire_runtime_idle_lease(&state, "reset_session")?;
    let system_prompt = current_runtime_system_prompt(&state).await;
    let mut conv = state.runtime_service.lock_conversation().await;
    *conv = muse_core::domain::conversation::Conversation::new(
        system_prompt,
        state.config.agent.max_history,
    );
    drop(conv);
    replace_runtime_todos(&state, Vec::new()).await;
    let conversation_id = next_runtime_session_id();
    let _ = set_active_conversation_id(&state, &conversation_id);
    finish_runtime_idle_lease(idle_lease)?;
    state.runtime_service.touch();
    Ok(Json(serde_json::json!({
        "status": "ok",
        "conversation_id": conversation_id,
    })))
}

/// 查询当前聊天模型概要。
pub(crate) async fn handle_models(State(state): State<Arc<AppState>>) -> Json<ModelsResponse> {
    let config = state.model_config.lock().await;
    let provider_id = config.chat().provider.clone();
    let model = config.chat().model.clone();
    let provider_name = config
        .model_provider(&provider_id)
        .map(|provider| provider.name)
        .or_else(|| (!provider_id.trim().is_empty()).then_some(provider_id.clone()))
        .unwrap_or_else(|| "未配置".to_string());
    let model_name = config
        .catalog_model(&provider_id, &model)
        .map(|catalog_model| catalog_model.name)
        .or_else(|| (!model.trim().is_empty()).then_some(model.clone()))
        .unwrap_or_else(|| "未选择模型".to_string());
    Json(ModelsResponse {
        provider: provider_name.clone(),
        model,
        provider_id,
        provider_name,
        model_name,
    })
}

/// 查询模型能力目录。
pub(crate) async fn handle_models_catalog(
    State(state): State<Arc<AppState>>,
) -> Result<Json<muse_core::model::catalog::ModelCatalog>, (StatusCode, Json<ErrorResponse>)> {
    load_verified_model_catalog(&state).await.map(Json)
}

/// 新增一个仅属于现有供应商的聊天模型。
pub(crate) async fn handle_create_catalog_model(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ModelCatalogMutationRequest>,
) -> Result<Json<muse_core::model::catalog::ModelCatalogItem>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.model_configuration_transition_gate.lock().await;
    state
        .model_config
        .lock()
        .await
        .create_catalog_model(request)
        .map(Json)
        .map_err(model_catalog_error_response)
}

/// 编辑模型名称和能力参数；供应商与模型 ID 不允许通过编辑迁移。
pub(crate) async fn handle_update_catalog_model(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ModelCatalogMutationRequest>,
) -> Result<Json<muse_core::model::catalog::ModelCatalogItem>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.model_configuration_transition_gate.lock().await;
    state
        .model_config
        .lock()
        .await
        .update_catalog_model(request)
        .map(Json)
        .map_err(model_catalog_error_response)
}

/// 删除模型目录项。当前活动模型必须先在聊天页切换后才能删除。
pub(crate) async fn handle_delete_catalog_model(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ModelCatalogDeleteRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.model_configuration_transition_gate.lock().await;
    {
        let mut config = state.model_config.lock().await;
        if config.chat().provider == request.provider_id && config.chat().model == request.model {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "active_model_conflict：当前对话正在使用该模型，请先在聊天页切换模型。"
                        .to_string(),
                }),
            ));
        }
        config
            .disable_catalog_model(&request.provider_id, &request.model)
            .map_err(model_catalog_error_response)?;
    }
    Ok(Json(StatusResponse {
        status: "模型已删除。".to_string(),
    }))
}

async fn load_verified_model_catalog(
    state: &Arc<AppState>,
) -> Result<muse_core::model::catalog::ModelCatalog, (StatusCode, Json<ErrorResponse>)> {
    refresh_model_config_from_disk(state).await?;
    Ok(state.model_config.lock().await.model_catalog())
}

/// 观察手工编辑的 `config.toml`，并在 revision 变化时热重建运行时提供器。
async fn refresh_model_config_from_disk(
    state: &Arc<AppState>,
) -> Result<ModelsConfig, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.model_configuration_transition_gate.lock().await;
    let (changed, models) = {
        let mut store = state.model_config.lock().await;
        let changed = store.refresh_from_disk().map_err(|_| {
            internal_error("读取 config.toml 失败，请检查语法、权限和磁盘状态。".to_string())
        })?;
        (changed, store.config().clone())
    };
    if changed {
        *state.provider.lock().await = build_chat_provider(&models.chat)
            .map_err(|error| bad_request(&error.to_string()))?;
        rebuild_tts_provider_from_state(state).await;
        rebuild_speech_provider_from_state(state).await;
    }
    Ok(models)
}

/// 更新供应商级 API Key。密钥只进入受保护的 `config.toml` Provider Profile。
pub(crate) async fn handle_put_provider_credential(
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<String>,
    Json(request): Json<SecretUpdate>,
) -> Result<Json<ProviderCredentialResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.model_configuration_transition_gate.lock().await;
    let provider = state
        .model_config
        .lock()
        .await
        .model_provider(&provider_id)
        .ok_or_else(|| bad_request("当前供应商不存在或已停用。"))?;

    if request.action == SecretUpdateAction::Keep {
        if request.value.is_some() {
            return Err(bad_request("密钥 action 为 keep 时不得提交 value。"));
        }
        let configured = provider.api_key_configured;
        return Ok(Json(ProviderCredentialResponse {
            provider_id,
            api_key_configured: configured,
            status: if configured {
                "供应商凭据已配置。"
            } else {
                "供应商凭据未配置。"
            }
            .to_string(),
            credential_diagnostic: None,
        }));
    }
    let next_secret = match request.action {
        SecretUpdateAction::Replace => Some(
            request
                .value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| bad_request("action 为 replace 时必须提交非空 API Key。"))?
                .to_string(),
        ),
        SecretUpdateAction::Delete => {
            if request.value.is_some() {
                return Err(bad_request("密钥 action 为 delete 时不得提交 value。"));
            }
            None
        }
        SecretUpdateAction::Keep => unreachable!("keep 已提前返回"),
    };
    let (configured, active_config) = {
        let mut store = state.model_config.lock().await;
        let configured = store
            .update_provider_api_key(&provider_id, next_secret)
            .map_err(model_catalog_error_response)?;
        (configured, store.config().clone())
    };
    *state.provider.lock().await = build_chat_provider(&active_config.chat)
        .map_err(|error| bad_request(&error.to_string()))?;

    Ok(Json(ProviderCredentialResponse {
        provider_id,
        api_key_configured: configured,
        status: if configured {
            "供应商凭据已保存。"
        } else {
            "供应商凭据已删除。"
        }
        .to_string(),
        credential_diagnostic: None,
    }))
}

/// 切换后续对话使用的供应商与模型。该操作不修改模型目录或其他供应商凭据。
pub(crate) async fn handle_put_active_chat_model(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ActiveChatModelUpdateRequest>,
) -> Result<Json<ActiveChatModelResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.model_configuration_transition_gate.lock().await;
    let (provider, model, next_provider) = {
        let mut store = state.model_config.lock().await;
        let provider = store
            .model_provider(&request.provider_id)
            .ok_or_else(|| bad_request("当前供应商不存在或已停用。"))?;
        let model = store
            .catalog_model(&request.provider_id, &request.model)
            .ok_or_else(|| bad_request("当前模型不存在或已删除。"))?;
        let next_config = store
            .update_active_chat_model(&request.provider_id, &request.model)
            .map_err(model_catalog_error_response)?;
        let built = build_chat_provider(&next_config.chat)
            .map_err(|error| bad_request(&error.to_string()))?;
        (provider, model, built)
    };
    *state.provider.lock().await = next_provider;

    Ok(Json(ActiveChatModelResponse {
        provider_id: provider.id,
        provider_name: provider.name,
        model: model.model,
        model_name: model.name,
    }))
}

/// 从提供器的模型列表接口或本地模型表读取当前可用模型。
///
/// 地址是否可用由提供器表按用途决定；没有地址时只读模型表。
pub(crate) async fn handle_fetch_model_catalog(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FetchModelCatalogRequest>,
) -> Result<Json<FetchModelCatalogResponse>, (StatusCode, Json<ErrorResponse>)> {
    let provider = req.provider.trim().to_string();
    let purpose = req.purpose.trim().to_string();
    let requested_api_base = req.api_base.trim().trim_end_matches('/').to_string();

    if provider.is_empty() {
        return Err(bad_request("请先选择提供商。"));
    }
    if requested_api_base.is_empty() {
        return Err(bad_request("API Base 不能为空，无法获取模型列表。"));
    }

    let catalog = load_verified_model_catalog(&state).await?;
    let provider_catalog = catalog
        .providers
        .iter()
        .find(|item| item.id == provider)
        .ok_or_else(|| bad_request("当前提供商不在模型目录中。"))?;
    let api_base = if provider_catalog.allow_custom_base {
        requested_api_base
    } else {
        provider_catalog.default_api_base.clone()
    };
    let model_list_url = model_list_url_for_purpose(provider_catalog, &purpose);

    let api_key = match req.api_key {
        Some(key) if !key.trim().is_empty() && !key.contains("****") => Some(key),
        _ if !provider_catalog.api_key.trim().is_empty() => Some(provider_catalog.api_key.clone()),
        _ => stored_api_key_for_provider(&state, &provider, &purpose).await,
    };
    if provider_catalog.model_list_auth == "required" && api_key.is_none() {
        return Err(bad_request(
            "provider_api_key_required：请先配置 API Key，再验证连接或加载模型列表。",
        ));
    }

    let local_models = catalog
        .models
        .into_iter()
        .filter(|model| model.enabled && model.provider_id == provider)
        .collect::<Vec<_>>();

    if provider_catalog.connection_validation == "chat_probe" {
        let probe_model = req
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| local_models.first().map(|model| model.model.clone()))
            .ok_or_else(|| bad_request("当前提供商没有可用于连接验证的模型。"))?;
        let api_key = api_key
            .as_deref()
            .ok_or_else(|| bad_request("请先配置 API Key，再验证连接。"))?;
        probe_provider_chat(&api_base, &probe_model, api_key).await?;
    }

    let models = if model_list_url.is_empty() {
        local_models_for_purpose(
            local_models,
            &purpose,
            "提供商未配置该用途的模型列表 URL，已读取本地 models 表。",
        )
    } else {
        match fetch_provider_models(
            &provider,
            &purpose,
            &api_base,
            &model_list_url,
            api_key.as_deref(),
        )
        .await
        {
            Ok(models) => {
                merge_remote_models_with_catalog(&purpose, models, local_models, &api_base)
            }
            Err(err) => return Err(err),
        }
    };

    Ok(Json(FetchModelCatalogResponse {
        provider_id: provider,
        models,
    }))
}

async fn probe_provider_chat(
    api_base: &str,
    model: &str,
    api_key: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| internal_error("创建供应商连接验证客户端失败。".to_string()))?;
    let response = client
        .post(format!(
            "{}/chat/completions",
            api_base.trim_end_matches('/')
        ))
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "回复 OK" }],
            "thinking": { "type": "disabled" },
            "stream": false,
            "max_tokens": 1,
        }))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    Json(ErrorResponse {
                        error: "provider_timeout：供应商连接验证超时。".to_string(),
                    }),
                )
            } else {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse {
                        error: "provider_unreachable：无法连接供应商对话接口。".to_string(),
                    }),
                )
            }
        })?;
    let status = response.status();
    if !status.is_success() {
        let upstream_detail = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|payload| safe_provider_error_detail(&payload));
        let (response_status, code) = match status.as_u16() {
            400 | 422 => (StatusCode::BAD_GATEWAY, "provider_invalid_request"),
            401 | 403 => (StatusCode::BAD_GATEWAY, "provider_auth_failed"),
            404 => (StatusCode::BAD_GATEWAY, "provider_not_found"),
            429 => (StatusCode::TOO_MANY_REQUESTS, "provider_rate_limited"),
            _ => (StatusCode::BAD_GATEWAY, "provider_unreachable"),
        };
        return Err((
            response_status,
            Json(ErrorResponse {
                error: format!(
                    "{code}：供应商连接验证返回 HTTP {status}{}。",
                    upstream_detail
                        .map(|detail| format!("（上游分类：{detail}）"))
                        .unwrap_or_default()
                ),
            }),
        ));
    }
    let payload = response.json::<serde_json::Value>().await.map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: "provider_protocol_error：连接验证响应不是有效 JSON。".to_string(),
            }),
        )
    })?;
    if payload.get("error").is_some_and(|error| !error.is_null()) {
        let detail = safe_provider_error_detail(&payload)
            .map(|value| format!("（上游分类：{value}）"))
            .unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: format!(
                    "provider_protocol_error：供应商在成功响应中返回错误对象{detail}。"
                ),
            }),
        ));
    }
    if payload.pointer("/choices/0/message").is_none() {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: "provider_protocol_error：连接验证响应缺少助手消息。".to_string(),
            }),
        ));
    }
    Ok(())
}

/// 连接验证只展示供应商公开的短分类字段，禁止把错误正文、URL 或响应头传给前端。
fn safe_provider_error_detail(payload: &serde_json::Value) -> Option<String> {
    let error = payload.get("error").unwrap_or(payload);
    let labels = [
        ("type", error.get("type").and_then(serde_json::Value::as_str)),
        ("code", error.get("code").and_then(serde_json::Value::as_str)),
    ]
    .into_iter()
    .filter_map(|(name, value)| {
        let value = value?.trim();
        if value.is_empty()
            || value.len() > 80
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return None;
        }
        Some(format!("{name}={value}"))
    })
    .collect::<Vec<_>>();
    (!labels.is_empty()).then(|| labels.join("，"))
}

/// 调用供应商专属能力：余额检测。
pub(crate) async fn handle_provider_balance(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ProviderBalanceRequest>,
) -> Result<Json<ProviderBalanceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let provider = req.provider.trim().to_string();
    let api_base = req.api_base.trim().trim_end_matches('/').to_string();
    let purpose = req
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("chat")
        .to_string();

    if provider.is_empty() {
        return Err(bad_request("请先选择提供商。"));
    }
    if api_base.is_empty() {
        return Err(bad_request("API Base 不能为空，无法检测余额。"));
    }

    let catalog = load_verified_model_catalog(&state).await?;
    let provider_catalog = catalog
        .providers
        .iter()
        .find(|item| item.id == provider)
        .ok_or_else(|| bad_request("当前提供商不在模型目录中。"))?;
    let support = provider_support_capabilities(&provider, &api_base);
    if !support.supports_balance_check {
        return Err(bad_request("当前提供商暂未实现余额检测。"));
    }

    let api_key = match req.api_key {
        Some(key) if !key.trim().is_empty() && !key.contains("****") => Some(key),
        _ if !provider_catalog.api_key.trim().is_empty() => Some(provider_catalog.api_key.clone()),
        _ => stored_api_key_for_provider(&state, &provider, &purpose).await,
    }
    .map(|key| key.trim().to_string())
    .filter(|key| !key.is_empty())
    .ok_or_else(|| bad_request("请先配置 API Key，再检测余额。"))?;

    let balance = fetch_provider_balance(&provider, &api_base, &api_key)
        .await
        .map_err(provider_support_error_response)?;
    let status = if balance.is_available {
        "供应商余额可用。"
    } else {
        "供应商余额不足或不可用。"
    }
    .to_string();

    Ok(Json(ProviderBalanceResponse {
        provider_id: balance.provider_id,
        is_available: balance.is_available,
        balance_infos: balance.balance_infos,
        status,
    }))
}

fn provider_support_error_response(err: ProviderSupportError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        ProviderSupportError::Unsupported(message) | ProviderSupportError::Config(message) => {
            bad_request(&message)
        }
        ProviderSupportError::Network(message) => {
            let (status, code) = if message.contains("超时") {
                (StatusCode::GATEWAY_TIMEOUT, "provider_timeout")
            } else {
                (StatusCode::BAD_GATEWAY, "provider_unreachable")
            };
            (
                status,
                Json(ErrorResponse {
                    error: format!("{code}：{message}"),
                }),
            )
        }
        ProviderSupportError::Api { status, message } => {
            let (http_status, code) = match status {
                401 | 403 => (StatusCode::BAD_GATEWAY, "provider_auth_failed"),
                404 => (StatusCode::BAD_GATEWAY, "provider_not_found"),
                429 => (StatusCode::TOO_MANY_REQUESTS, "provider_rate_limited"),
                _ => (StatusCode::BAD_GATEWAY, "provider_unreachable"),
            };
            (
                http_status,
                Json(ErrorResponse {
                    error: format!("{code}：{message}"),
                }),
            )
        }
        ProviderSupportError::Parse(message) => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: format!("provider_protocol_error：{message}"),
            }),
        ),
    }
}

fn model_list_url_for_purpose(
    provider: &muse_core::model::catalog::ModelProviderCatalog,
    purpose: &str,
) -> String {
    match purpose {
        "tts" => provider.tts_model_list_url.trim().to_string(),
        _ => provider.chat_model_list_url.trim().to_string(),
    }
}

fn local_models_for_purpose(
    models: Vec<muse_core::model::catalog::ModelCatalogItem>,
    purpose: &str,
    notes: &str,
) -> Vec<muse_core::model::catalog::ModelCatalogItem> {
    models
        .into_iter()
        .filter(|model| model.functions.iter().any(|item| item == purpose))
        .map(|mut model| {
            model.notes = notes.to_string();
            model
        })
        .collect()
}

fn merge_remote_models_with_catalog(
    purpose: &str,
    remote_models: Vec<muse_core::model::catalog::ModelCatalogItem>,
    local_models: Vec<muse_core::model::catalog::ModelCatalogItem>,
    api_base: &str,
) -> Vec<muse_core::model::catalog::ModelCatalogItem> {
    remote_models
        .into_iter()
        .map(|remote| {
            let matched = local_models
                .iter()
                .find(|local| local.model.trim().eq_ignore_ascii_case(remote.model.trim()));
            let mut model = matched.cloned().unwrap_or(remote);
            model.default_api_base = api_base.to_string();
            if model.functions.is_empty() {
                model.functions = vec![purpose.to_string()];
            }
            model.capabilities = merge_model_labels(&model.functions, &model.tags);
            model.notes = if matched.is_some() {
                "来自提供商模型列表接口，并已合并 models 表中的标签。".to_string()
            } else {
                "来自提供商模型列表接口；models 表未记录额外标签。".to_string()
            };
            model
        })
        .collect()
}

fn merge_model_labels(functions: &[String], tags: &[String]) -> Vec<String> {
    let mut labels = functions
        .iter()
        .chain(tags.iter())
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels
}

fn keep_non_empty(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn normalize_model_api_protocol(_value: String, _fallback: &str) -> String {
    "chat_completions".to_string()
}

async fn stored_api_key_for_provider(
    state: &Arc<AppState>,
    provider_id: &str,
    purpose: &str,
) -> Option<String> {
    let cfg = state.model_config.lock().await;
    if let Some(provider) = cfg.model_provider(provider_id) {
        let key = provider.api_key.trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    match purpose {
        "chat" => (cfg.chat().provider == provider_id)
                .then(|| cfg.chat().api_key.as_ref())
                .flatten()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        "audio_understanding" => cfg
            .audio_understanding()
            .api_key
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        _ => None,
    }
}

async fn fetch_provider_models(
    provider: &str,
    purpose: &str,
    api_base: &str,
    model_list_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<muse_core::model::catalog::ModelCatalogItem>, (StatusCode, Json<ErrorResponse>)>
{
    let url = model_list_url.trim().to_string();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| internal_error("创建模型列表请求客户端失败。".to_string()))?;

    let mut request = client.get(&url);
    if let Some(key) = api_key {
        if provider == "anthropic" {
            request = request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
        } else {
            request = request.bearer_auth(key);
        }
    }

    let response = request.send().await.map_err(|error| {
        let (status, code, message) = if error.is_timeout() {
            (
                StatusCode::GATEWAY_TIMEOUT,
                "provider_timeout",
                "获取提供商模型列表超时，请稍后重试。",
            )
        } else {
            (
                StatusCode::BAD_GATEWAY,
                "provider_unreachable",
                "无法连接提供商模型列表接口。",
            )
        };
        (
            status,
            Json(ErrorResponse {
                error: format!("{code}：{message}"),
            }),
        )
    })?;
    let status = response.status();

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: "provider_auth_failed：提供商拒绝了当前凭据，请检查 API Key。"
                    .to_string(),
            }),
        ));
    }
    if !status.is_success() {
        let (response_status, code) = match status.as_u16() {
            404 => (StatusCode::BAD_GATEWAY, "provider_not_found"),
            429 => (StatusCode::TOO_MANY_REQUESTS, "provider_rate_limited"),
            _ => (StatusCode::BAD_GATEWAY, "provider_unreachable"),
        };
        return Err((
            response_status,
            Json(ErrorResponse {
                error: format!("{code}：提供商模型列表接口返回 HTTP {status}。"),
            }),
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|_| internal_error("读取模型列表响应失败。".to_string()))?;

    let payload: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: "provider_protocol_error：模型列表响应不是有效 JSON。".to_string(),
            }),
        )
    })?;
    let ids = extract_model_ids(&payload);
    if ids.is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: "provider_protocol_error：提供商返回中没有识别到模型列表。"
                    .to_string(),
            }),
        ));
    }

    let models = ids
        .into_iter()
        .map(|id| {
            let defaults = model_capability_defaults(provider, api_base, &id);
            muse_core::model::catalog::ModelCatalogItem {
                id: format!("remote-{}-{}", provider, sanitize_model_id(&id)),
                provider_id: provider.to_string(),
                name: id.clone(),
                model: id,
                default_api_base: api_base.to_string(),
                enabled: true,
                notes: "来自提供商模型列表接口，能力由本地 provider profile 兜底。".to_string(),
                tags: Vec::new(),
                functions: vec![purpose.to_string()],
                capabilities: vec![purpose.to_string()],
                context_window: defaults.context_window,
                default_max_output_tokens: defaults.default_max_output_tokens,
                supports_usage: defaults.supports_usage,
                supports_cached_tokens: defaults.supports_cached_tokens,
                supports_reasoning_tokens: defaults.supports_reasoning_tokens,
                tokenizer_family: "rough_estimate".to_string(),
            }
        })
        .collect();
    Ok(models)
}

fn extract_model_ids(payload: &serde_json::Value) -> Vec<String> {
    let mut ids = Vec::new();
    collect_model_ids(payload.get("data"), &mut ids);
    collect_model_ids(payload.get("models"), &mut ids);
    collect_model_ids(payload.pointer("/data/models"), &mut ids);
    ids.sort();
    ids.dedup();
    ids
}

fn collect_model_ids(value: Option<&serde_json::Value>, ids: &mut Vec<String>) {
    let Some(serde_json::Value::Array(items)) = value else {
        return;
    };
    for item in items {
        match item {
            serde_json::Value::String(id) => push_model_id(ids, id),
            serde_json::Value::Object(object) => {
                for key in ["id", "model", "name", "display_name"] {
                    if let Some(serde_json::Value::String(id)) = object.get(key) {
                        push_model_id(ids, id);
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

fn push_model_id(ids: &mut Vec<String>, id: &str) {
    let trimmed = id.trim();
    if !trimmed.is_empty() {
        ids.push(trimmed.to_string());
    }
}

fn sanitize_model_id(model: &str) -> String {
    model
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn uploaded_assets_dir() -> PathBuf {
    muse_core::config::Config::config_dir()
        .join("assets")
        .join("uploaded")
}

async fn persist_uploaded_asset(path: &StdPath, bytes: &[u8]) -> Result<(), String> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
    {
        Ok(mut file) => {
            let result = async {
                file.write_all(bytes)
                    .await
                    .map_err(|error| format!("保存角色图片失败：{error}"))?;
                file.sync_data()
                    .await
                    .map_err(|error| format!("同步角色图片失败：{error}"))
            }
            .await;
            drop(file);
            if result.is_err() {
                let _ = fs::remove_file(path).await;
            }
            result
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .await
                .map_err(|error| format!("检查已有角色图片失败：{error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("角色图片目标已被非普通文件占用。".to_string());
            }
            let existing = fs::read(path)
                .await
                .map_err(|error| format!("读取已有角色图片失败：{error}"))?;
            if existing != bytes {
                return Err("已有角色图片与内容哈希不一致。".to_string());
            }
            Ok(())
        }
        Err(error) => Err(format!("创建角色图片失败：{error}")),
    }
}

fn validated_uploaded_asset_name(filename: &str) -> Option<(&str, &str)> {
    let (digest, extension) = filename.rsplit_once('.')?;
    let valid_digest = digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    (valid_digest && matches!(extension, "png" | "jpg" | "webp")).then_some((digest, extension))
}

fn uploaded_image_extension(content_type: Option<&str>) -> Option<&'static str> {
    match content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

fn uploaded_image_extension_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

fn content_hash_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 查询当前运行时模型配置。
pub(crate) async fn handle_get_models_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ModelsConfigResponse>, (StatusCode, Json<ErrorResponse>)> {
    let cfg = refresh_model_config_from_disk(&state).await?;
    let chat = &cfg.chat;
    let tts = &cfg.tts;
    let speech_recognition = &cfg.speech_recognition;
    let audio_understanding = &cfg.audio_understanding;
    let voice_input = &cfg.voice_input;
    Ok(Json(ModelsConfigResponse {
        chat: ConfigSectionResponse {
            provider: chat.provider.clone(),
            api_base: chat.api_base.clone(),
            api_protocol: chat.api_protocol.clone(),
            api_key_configured: config_has_api_key(chat.api_key.as_deref()),
            model: chat.model.clone(),
            max_tokens: Some(chat.max_tokens),
            temperature: Some(chat.temperature),
            voice_id: None,
            speed: None,
        },
        tts: TtsConfigResponse {
            enabled: tts.enabled,
            provider: tts.provider.clone(),
            api_base: tts.api_base.clone(),
            api_key_configured: config_has_api_key(tts.api_key.as_deref()),
            model: tts.model.clone(),
            voice_id: tts.voice_id.clone(),
            speed: tts.speed,
            response_format: tts.response_format.clone(),
        },
        speech_recognition: SpeechRecognitionConfigResponse {
            enabled: speech_recognition.enabled,
            provider: speech_recognition.provider.clone(),
            api_base: speech_recognition.api_base.clone(),
            api_key_configured: config_has_api_key(speech_recognition.api_key.as_deref()),
            model: speech_recognition.model.clone(),
            language: speech_recognition.language.clone(),
            response_format: speech_recognition.response_format.clone(),
        },
        audio_understanding: ConfigSectionResponse {
            provider: audio_understanding.provider.clone(),
            api_base: audio_understanding.api_base.clone(),
            api_protocol: audio_understanding.api_protocol.clone(),
            api_key_configured: config_has_api_key(audio_understanding.api_key.as_deref()),
            model: audio_understanding.model.clone(),
            max_tokens: Some(audio_understanding.max_tokens),
            temperature: Some(audio_understanding.temperature),
            voice_id: None,
            speed: None,
        },
        voice_input: VoiceInputConfigResponse {
            mode: voice_input.mode.clone(),
        },
    }))
}

fn config_has_api_key(value: Option<&str>) -> bool {
    value.is_some_and(|key| !key.trim().is_empty())
}

/// 读取 Brave Search 凭据的配置状态，不向界面泄漏密钥内容或掩码。
pub(crate) async fn handle_get_web_search_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<WebSearchConfigResponse>, (StatusCode, Json<ErrorResponse>)> {
    let api_key_configured = state
        .secrets
        .get_optional("web-search.brave")
        .map_err(|err| internal_error(err.to_string()))?
        .is_some();
    Ok(Json(WebSearchConfigResponse {
        provider: "brave_search_api".to_string(),
        api_key_configured,
    }))
}

/// 更新 Brave Search 密钥。密钥只进入系统凭据库，配置文件与响应体都不保留明文。
pub(crate) async fn handle_put_web_search_config(
    State(state): State<Arc<AppState>>,
    Json(request): Json<WebSearchConfigUpdate>,
) -> Result<Json<WebSearchConfigResponse>, (StatusCode, Json<ErrorResponse>)> {
    match request.action {
        SecretUpdateAction::Keep => {
            if request.value.is_some() {
                return Err(bad_request("action 为 keep 时不得提交 value"));
            }
        }
        SecretUpdateAction::Replace => {
            let api_key = request
                .value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| bad_request("action 为 replace 时必须提交非空 value"))?;
            let previous = state
                .secrets
                .get_optional("web-search.brave")
                .map_err(|err| internal_error(err.to_string()))?;
            if let Err(error) = state.secrets.set_verified("web-search.brave", api_key) {
                let rollback = match previous {
                    Some(previous) => state
                        .secrets
                        .set_verified("web-search.brave", &previous),
                    None => state.secrets.delete("web-search.brave"),
                };
                if let Err(rollback_error) = rollback {
                    return Err(internal_error(format!(
                        "替换联网搜索凭据失败，且原凭据回滚失败：{error}；{rollback_error}"
                    )));
                }
                return Err(internal_error(format!(
                    "替换联网搜索凭据失败，原凭据保持不变：{error}"
                )));
            }
        }
        SecretUpdateAction::Delete => {
            if request.value.is_some() {
                return Err(bad_request("action 为 delete 时不得提交 value"));
            }
            state
                .secrets
                .delete("web-search.brave")
                .map_err(|err| internal_error(err.to_string()))?;
        }
    }

    handle_get_web_search_config(State(state)).await
}

struct ResolvedSettingSecretUpdate {
    runtime_value: Option<String>,
}

fn normalize_chat_max_tokens(provider: &str, requested: u32) -> u32 {
    provider_profile_for_identity(provider, "")
        .map(|profile| requested.clamp(1, profile.model_defaults.default_max_output_tokens))
        .unwrap_or(requested)
}

/// 更新运行时模型配置并热重建相关提供器。
pub(crate) async fn handle_put_models_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ModelsConfigUpdate>,
) -> Result<Json<ModelsConfigResponse>, (StatusCode, Json<ErrorResponse>)> {
    use muse_core::model::config::LlmConfig;

    let _transition = state.model_configuration_transition_gate.lock().await;

    // 合并请求体与现有存储：`api_key` 为空时保持原值。
    let ModelsConfigUpdate {
        chat: chat_req,
        tts: tts_req,
        speech_recognition: speech_req,
        audio_understanding: audio_understanding_req,
        voice_input: voice_input_req,
    } = req;

    let (new_config, new_provider) = {
        let mut store = state.model_config.lock().await;
        let existing = store.config().clone();

        let requested_chat_provider = chat_req.provider.trim().to_ascii_lowercase();
        let supported_chat_profile =
            provider_profile_for_identity(&requested_chat_provider, "");
        let legacy_chat_unchanged = requested_chat_provider
            == existing.chat.provider.trim().to_ascii_lowercase()
            && chat_req.api_base == existing.chat.api_base
            && chat_req.model == existing.chat.model
            && chat_req.max_tokens.unwrap_or(existing.chat.max_tokens)
                == existing.chat.max_tokens
            && chat_req.temperature.unwrap_or(existing.chat.temperature)
                == existing.chat.temperature
            && normalize_model_api_protocol(
                chat_req.api_protocol.clone(),
                &existing.chat.api_protocol,
            ) == existing.chat.api_protocol
            && chat_req.api_key.as_deref().is_none_or(|value| value.is_empty())
            && chat_req.api_key_update.action == SecretUpdateAction::Keep;
        if !requested_chat_provider.is_empty()
            && supported_chat_profile.is_none()
            && !legacy_chat_unchanged
        {
            return Err(bad_request(
                "当前聊天供应商已停止支持；旧配置只能原样保留，请切换到 DeepSeek 或火山方舟 Agent Plan。",
            ));
        }
        let normalized_chat_api_base = supported_chat_profile
            .map(|profile| profile.default_api_base.to_string())
            .unwrap_or_else(|| chat_req.api_base.clone());
        let normalized_chat_provider = if supported_chat_profile.is_some()
            || requested_chat_provider.is_empty()
        {
            requested_chat_provider
        } else {
            existing.chat.provider.clone()
        };

        let chat_secret = resolve_setting_secret_update(
            &chat_req.api_key_update,
            chat_req.api_key.as_deref(),
            existing.chat.api_key.clone(),
        )?;
        let tts_secret = resolve_setting_secret_update(
            &tts_req.api_key_update,
            tts_req.api_key.as_deref(),
            existing.tts.api_key.clone(),
        )?;
        let asr_secret = resolve_setting_secret_update(
            &speech_req.api_key_update,
            speech_req.api_key.as_deref(),
            existing.speech_recognition.api_key.clone(),
        )?;
        let audio_secret = resolve_setting_secret_update(
            &audio_understanding_req.api_key_update,
            audio_understanding_req.api_key.as_deref(),
            existing.audio_understanding.api_key.clone(),
        )?;

        let requested_chat_max_tokens = chat_req.max_tokens.unwrap_or(existing.chat.max_tokens);
        let normalized_chat_max_tokens =
            normalize_chat_max_tokens(&normalized_chat_provider, requested_chat_max_tokens);
        let chat = LlmConfig {
            provider: normalized_chat_provider,
            api_base: normalized_chat_api_base,
            api_key: chat_secret.runtime_value,
            api_protocol: normalize_model_api_protocol(
                chat_req.api_protocol,
                &existing.chat.api_protocol,
            ),
            model: chat_req.model,
            max_tokens: normalized_chat_max_tokens,
            temperature: chat_req.temperature.unwrap_or(existing.chat.temperature),
        };
        let mut tts = TtsConfig {
            enabled: tts_req.enabled,
            provider: keep_non_empty(tts_req.provider, &existing.tts.provider),
            api_base: keep_non_empty(tts_req.api_base, &existing.tts.api_base),
            api_key: tts_secret.runtime_value,
            model: keep_non_empty(tts_req.model, &existing.tts.model),
            voice_id: keep_non_empty(tts_req.voice_id, &existing.tts.voice_id),
            speed: tts_req.speed.unwrap_or(existing.tts.speed),
            response_format: keep_non_empty(tts_req.response_format, &existing.tts.response_format),
        };
        tts.ensure_default_profiles();

        let mut speech_recognition = SpeechRecognitionConfig {
            enabled: speech_req.enabled,
            provider: keep_non_empty(speech_req.provider, &existing.speech_recognition.provider),
            api_base: keep_non_empty(speech_req.api_base, &existing.speech_recognition.api_base),
            api_key: asr_secret.runtime_value,
            model: keep_non_empty(speech_req.model, &existing.speech_recognition.model),
            language: keep_non_empty(speech_req.language, &existing.speech_recognition.language),
            response_format: keep_non_empty(
                speech_req.response_format,
                &existing.speech_recognition.response_format,
            ),
        };
        speech_recognition.ensure_openai_compatible();
        let audio_understanding = LlmConfig {
            provider: audio_understanding_req.provider,
            api_base: audio_understanding_req.api_base,
            api_key: audio_secret.runtime_value,
            api_protocol: normalize_model_api_protocol(
                audio_understanding_req.api_protocol,
                &existing.audio_understanding.api_protocol,
            ),
            model: audio_understanding_req.model,
            max_tokens: audio_understanding_req
                .max_tokens
                .unwrap_or(existing.audio_understanding.max_tokens),
            temperature: audio_understanding_req
                .temperature
                .unwrap_or(existing.audio_understanding.temperature),
        };
        let requested_voice_mode = voice_input_req.mode.trim();
        if !requested_voice_mode.is_empty() && requested_voice_mode != "speech_text" {
            return Err(bad_request(
                "语音输入模式仅支持 `speech_text`；旧版本地识别和音频理解模式已停用。",
            ));
        }
        let voice_input = VoiceInputConfig {
            mode: "speech_text".to_string(),
        };

        let new_config = ModelsConfig {
            chat,
            tts,
            speech_recognition,
            audio_understanding,
            voice_input,
            mcp_servers: BTreeMap::new(),
            secret_bindings: Default::default(),
        };
        let saved = store.update_models_config(new_config).map_err(|error| match error {
            muse_core::app::preferences::MuseConfigStoreError::Validation(diagnostic) => {
                bad_request(&diagnostic.message)
            }
            muse_core::app::preferences::MuseConfigStoreError::Conflict(diagnostic) => (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!("{}：{}", diagnostic.code, diagnostic.message),
                }),
            ),
            _ => internal_error(
                "保存 config.toml 失败，请检查文件权限和磁盘状态。".to_string(),
            ),
        })?;
        let new_provider =
            build_chat_provider(&saved.chat).map_err(|err| bad_request(&err.to_string()))?;
        (saved, new_provider)
    };

    // 热重建 provider：替换互斥锁内的共享指针。
    // 进行中的请求持有旧共享指针，自然跑完；新请求走新的真实 provider。
    {
        *state.provider.lock().await = new_provider;
    }
    rebuild_tts_provider_from_state(&state).await;
    rebuild_speech_provider_from_state(&state).await;

    // 只返回密钥配置状态，明文和掩码都不进入响应。
    let response = ModelsConfigResponse {
        chat: ConfigSectionResponse {
            provider: new_config.chat.provider.clone(),
            api_base: new_config.chat.api_base.clone(),
            api_protocol: new_config.chat.api_protocol.clone(),
            api_key_configured: new_config
                .chat
                .api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()),
            model: new_config.chat.model.clone(),
            max_tokens: Some(new_config.chat.max_tokens),
            temperature: Some(new_config.chat.temperature),
            voice_id: None,
            speed: None,
        },
        tts: TtsConfigResponse {
            enabled: new_config.tts.enabled,
            provider: new_config.tts.provider.clone(),
            api_base: new_config.tts.api_base.clone(),
            api_key_configured: new_config
                .tts
                .api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()),
            model: new_config.tts.model.clone(),
            voice_id: new_config.tts.voice_id.clone(),
            speed: new_config.tts.speed,
            response_format: new_config.tts.response_format.clone(),
        },
        speech_recognition: SpeechRecognitionConfigResponse {
            enabled: new_config.speech_recognition.enabled,
            provider: new_config.speech_recognition.provider.clone(),
            api_base: new_config.speech_recognition.api_base.clone(),
            api_key_configured: new_config
                .speech_recognition
                .api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()),
            model: new_config.speech_recognition.model.clone(),
            language: new_config.speech_recognition.language.clone(),
            response_format: new_config.speech_recognition.response_format.clone(),
        },
        audio_understanding: ConfigSectionResponse {
            provider: new_config.audio_understanding.provider.clone(),
            api_base: new_config.audio_understanding.api_base.clone(),
            api_protocol: new_config.audio_understanding.api_protocol.clone(),
            api_key_configured: new_config
                .audio_understanding
                .api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()),
            model: new_config.audio_understanding.model.clone(),
            max_tokens: Some(new_config.audio_understanding.max_tokens),
            temperature: Some(new_config.audio_understanding.temperature),
            voice_id: None,
            speed: None,
        },
        voice_input: VoiceInputConfigResponse {
            mode: new_config.voice_input.mode.clone(),
        },
    };
    Ok(Json(response))
}

fn resolve_setting_secret_update(
    update: &SecretUpdate,
    legacy_value: Option<&str>,
    current: Option<String>,
) -> Result<ResolvedSettingSecretUpdate, (StatusCode, Json<ErrorResponse>)> {
    let legacy_value = legacy_value
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if update.action != SecretUpdateAction::Keep && legacy_value.is_some() {
        return Err(bad_request(
            "api_key_update 与旧版 api_key 不能同时提交变更。",
        ));
    }
    match update.action {
        SecretUpdateAction::Keep => {
            if update.value.is_some() {
                return Err(bad_request("密钥 action 为 keep 时不得提交 value。"));
            }
            let legacy_value = legacy_value.map(ToString::to_string);
            Ok(ResolvedSettingSecretUpdate {
                // 旧版 api_key 的非空值仍表示替换，兼容旧客户端。
                runtime_value: legacy_value.or(current),
            })
        }
        SecretUpdateAction::Replace => {
            let value = update
                .value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| bad_request("密钥 action 为 replace 时必须提交非空 value。"))?;
            let value = value.to_string();
            Ok(ResolvedSettingSecretUpdate {
                runtime_value: Some(value),
            })
        }
        SecretUpdateAction::Delete => {
            if update.value.is_some() {
                return Err(bad_request("密钥 action 为 delete 时不得提交 value。"));
            }
            Ok(ResolvedSettingSecretUpdate {
                runtime_value: None,
            })
        }
    }
}

async fn resolve_tts_request_context(
    state: &Arc<AppState>,
    requested_voice_id: Option<&str>,
) -> Result<TtsRequestContext, (StatusCode, Json<ErrorResponse>)> {
    let mut effective_tts = {
        let config = state.model_config.lock().await;
        config.tts().clone()
    };
    if let Some(voice_id) = requested_voice_id
        .map(str::trim)
        .filter(|voice_id| !voice_id.is_empty())
    {
        effective_tts.voice_id = voice_id.to_string();
    }
    effective_tts = effective_tts_config(&effective_tts);
    if !effective_tts.is_external_provider() || !effective_tts.enabled() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "外接 TTS 服务未启用或配置不完整，请检查 Provider、Base URL、模型、音色与输出格式。"
                    .to_string(),
            }),
        ));
    }
    Ok(TtsRequestContext { effective_tts })
}
