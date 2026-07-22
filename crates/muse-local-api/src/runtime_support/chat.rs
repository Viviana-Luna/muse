//! 对话、会话与模型 HTTP handler 的历史实现集合。

use super::*;

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
    Ok(
        match runtime
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
        },
    )
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

pub(super) fn normalize_selected_skill(
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
    initialize_new_session_approval_mode(&state, &conversation_id)
        .await
        .map_err(internal_error)?;
    finish_runtime_idle_lease(idle_lease)?;
    state.runtime_service.touch();
    Ok(Json(serde_json::json!({
        "status": "ok",
        "conversation_id": conversation_id,
    })))
}
