//! 运行时交互与状态 HTTP handler。

use super::*;

/// 处理工具审批通过请求，并唤醒等待中的工具调用。
pub(crate) async fn handle_runtime_approval_approve(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RuntimeDecisionRequest>,
) -> Result<Json<ApprovalDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    resolve_runtime_approval(state, req.turn_id, id, true, None).await
}

/// 处理工具审批拒绝请求，并把拒绝原因返回给等待中的工具调用。
pub(crate) async fn handle_runtime_approval_reject(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<ApprovalRejectRequest>,
) -> Result<Json<ApprovalDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    resolve_runtime_approval(state, req.turn_id, id, false, req.reason).await
}

/// 处理工具审批取消请求，并按拒绝语义结束等待中的工具调用。
pub(crate) async fn handle_runtime_approval_cancel(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RuntimeDecisionRequest>,
) -> Result<Json<ApprovalDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    resolve_runtime_approval(
        state,
        req.turn_id,
        id,
        false,
        Some("用户取消本次工具执行。".to_string()),
    )
    .await
}

/// 处理用户问题回答请求，并唤醒等待中的交互式工具调用。
pub(crate) async fn handle_runtime_user_question_answer(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<UserQuestionAnswerRequest>,
) -> Result<Json<UserQuestionDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    if !req.answers.is_object() {
        return Err(bad_request("用户问题答案必须是对象。"));
    }
    if let Some(annotations) = &req.annotations
        && !annotations.is_object()
    {
        return Err(bad_request("用户问题附加说明必须是对象。"));
    }
    resolve_runtime_user_question(
        state,
        req.turn_id,
        id,
        true,
        Some(req.answers),
        req.annotations,
        None,
    )
    .await
}

/// 处理用户问题取消请求，并按失败工具结果恢复模型循环。
pub(crate) async fn handle_runtime_user_question_cancel(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RuntimeDecisionRequest>,
) -> Result<Json<UserQuestionDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    resolve_runtime_user_question(
        state,
        req.turn_id,
        id,
        false,
        None,
        None,
        Some("用户取消回答问题。".to_string()),
    )
    .await
}

/// 处理当前运行轮次取消请求。
pub(crate) async fn handle_runtime_turn_cancel(
    Path(turn_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let runtime = ConversationRuntime::new(state, false);
    let outcome = runtime
        .submit(
            RuntimeOp::CancelTurn {
                turn_id: turn_id.clone(),
            },
            RuntimeEventEmitter::collect_only(),
        )
        .await
        .map_err(|err| bad_request(&err))?;
    Ok(Json(StatusResponse {
        status: outcome.reply,
    }))
}

/// 查询运行底座工作区和权限策略。
pub(crate) async fn handle_runtime_workspaces(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<RuntimeWorkspacesResponse>, (StatusCode, Json<ErrorResponse>)> {
    runtime_workspaces_response()
        .map(Json)
        .map_err(runtime_workspace_error_response)
}

fn runtime_approval_mode_response(
    conversation_id: String,
    policy: &FrozenExecutionPolicy,
    status: impl Into<String>,
) -> RuntimeApprovalModeResponse {
    RuntimeApprovalModeResponse {
        conversation_id,
        preset: policy.preset().as_str().to_string(),
        approval_policy: match policy.approval_policy {
            ApprovalPolicy::OnRequest => "on_request",
            ApprovalPolicy::Never => "never",
        }
        .to_string(),
        approvals_reviewer: match policy.approvals_reviewer {
            ApprovalsReviewer::User => "user",
            ApprovalsReviewer::AutoReview => "auto_review",
        }
        .to_string(),
        permission_profile: match policy.permission_profile {
            PermissionProfile::WorkspaceWrite => "workspace_write",
            PermissionProfile::DangerFullAccess => "danger_full_access",
        }
        .to_string(),
        revision: policy.revision,
        status: status.into(),
    }
}

/// 查询当前活动会话的审批模式。
pub(crate) async fn handle_runtime_approval_mode(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RuntimeApprovalModeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let conversation_id = active_conversation_id(&state);
    let policy = state
        .runtime_service
        .execution_policy()
        .map_err(|error| internal_error(error.to_string()))?;
    Ok(Json(runtime_approval_mode_response(
        conversation_id,
        &policy,
        "ok",
    )))
}

/// 原子更新当前活动会话的审批模式；更新只在空闲期生效。
pub(crate) async fn handle_runtime_approval_mode_update(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RuntimeApprovalModeUpdateRequest>,
) -> Result<Json<RuntimeApprovalModeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let preset = ApprovalModePreset::parse(req.preset.trim())
        .ok_or_else(|| bad_request("未知审批模式。"))?;
    let active_persona = require_active_persona_http(&state).await?;
    let idle_lease = acquire_runtime_idle_lease(&state, "update_approval_mode")?;
    let conversation_id = active_conversation_id(&state);
    let current = state
        .runtime_service
        .execution_policy()
        .map_err(|error| internal_error(error.to_string()))?;
    if let Some(expected) = req.expected_revision
        && expected != current.revision
    {
        let _ = finish_runtime_idle_lease(idle_lease);
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "审批模式已经变化，请刷新后重试。".to_string(),
            }),
        ));
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    repository
        .ensure_persona_binding(
            &conversation_id,
            &active_persona.id,
            &active_persona.name,
            &active_persona.version,
        )
        .await
        .map_err(session_metadata_read_error)?;
    let saved = repository
        .update_approval_mode(&conversation_id, preset)
        .await
        .map_err(|error| internal_error(error.to_string()))?;
    let policy =
        FrozenExecutionPolicy::from_preset(saved.preset, current.allowed_roots, saved.revision);
    state
        .runtime_service
        .set_execution_policy(policy.clone())
        .map_err(|error| internal_error(error.to_string()))?;
    finish_runtime_idle_lease(idle_lease)?;
    Ok(Json(runtime_approval_mode_response(
        conversation_id,
        &policy,
        "updated",
    )))
}

/// 查询运行时 Token 用量聚合。
pub(crate) async fn handle_runtime_token_usage(
    Query(query): Query<RuntimeTokenUsageQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<RuntimeTokenUsageResponse>, (StatusCode, Json<ErrorResponse>)> {
    let conversation_id = query
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| Some(active_conversation_id(&state)));
    let range = normalize_token_usage_range(query.range.as_deref());
    let since = token_usage_since(&range);
    let store = RuntimeUsageStore::load_from_dir(muse_core::config::Config::config_dir())
        .map_err(|err| internal_error(err.to_string()))?;
    let items = store
        .list_token_usage(conversation_id.as_deref(), since.as_deref())
        .map_err(|err| internal_error(err.to_string()))?;
    let totals = runtime_token_usage_totals(&items);
    let by_source = runtime_token_usage_by_source(&items);
    let by_model = runtime_token_usage_by_model(&items);
    let records = items.len();
    Ok(Json(RuntimeTokenUsageResponse {
        conversation_id,
        range,
        from: since,
        to: chrono::Utc::now().to_rfc3339(),
        records,
        totals,
        by_source,
        by_model,
        items,
        status: "Token 用量已读取。".to_string(),
    }))
}

/// 查询当前会话最新上下文快照。
pub(crate) async fn handle_runtime_context_snapshot(
    Query(query): Query<RuntimeContextSnapshotQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<RuntimeContextSnapshotResponse>, (StatusCode, Json<ErrorResponse>)> {
    let conversation_id = query
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| active_conversation_id(&state));
    let store = RuntimeUsageStore::load_from_dir(muse_core::config::Config::config_dir())
        .map_err(|err| internal_error(err.to_string()))?;
    let snapshot = store
        .latest_context_snapshot(&conversation_id)
        .map_err(|err| internal_error(err.to_string()))?;
    Ok(Json(RuntimeContextSnapshotResponse {
        conversation_id,
        snapshot,
        status: "上下文快照已读取。".to_string(),
    }))
}

/// 查询单一运行时事实状态，供前端在重连或 mutation 后原子同步。
pub(crate) async fn handle_runtime_state(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RuntimeStateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _transition = state.persona_runtime_transition_gate.lock().await;
    for _ in 0..RUNTIME_FACT_SNAPSHOT_MAX_ATTEMPTS {
        let snapshot_before = state
            .runtime_service
            .snapshot()
            .map_err(|err| internal_error(err.to_string()))?;
        let active_persona_id = state
            .personas
            .lock()
            .await
            .active_persona_id()
            .map(ToString::to_string);
        let active_conversation_id = active_conversation_id(&state);
        let mode = current_runtime_mode_state(&state);
        let usage_store = RuntimeUsageStore::load_from_dir(muse_core::config::Config::config_dir())
            .map_err(|err| internal_error(err.to_string()))?;
        let usage_items = usage_store
            .list_token_usage(Some(&active_conversation_id), None)
            .map_err(|err| internal_error(err.to_string()))?;
        let context = usage_store
            .latest_context_snapshot(&active_conversation_id)
            .map_err(|err| internal_error(err.to_string()))?;
        let message_count = state
            .runtime_service
            .lock_conversation()
            .await
            .messages
            .len();
        let snapshot_after = state
            .runtime_service
            .snapshot()
            .map_err(|err| internal_error(err.to_string()))?;
        if snapshot_before != snapshot_after {
            tokio::task::yield_now().await;
            continue;
        }
        return Ok(Json(RuntimeStateResponse {
            state_revision: snapshot_after.state_revision,
            active_persona_id,
            active_conversation_id,
            mode: mode.mode.as_str().to_string(),
            focus_phase: mode.focus_phase.as_str().to_string(),
            busy_turn: snapshot_after
                .turn_id
                .map(|turn_id| RuntimeBusyTurnResponse {
                    turn_id,
                    phase: runtime_phase_name(snapshot_after.phase).to_string(),
                }),
            exclusive_operation: snapshot_after.exclusive_operation,
            usage_summary: serde_json::json!({
                "records": usage_items.len(),
                "totals": runtime_token_usage_totals(&usage_items),
            }),
            context_summary: serde_json::json!({
                "message_count": message_count,
                "latest": context,
            }),
        }));
    }
    Err((
        StatusCode::CONFLICT,
        Json(ErrorResponse {
            error: "runtime_snapshot_unstable：运行时事实正在连续变化，请重试。".to_string(),
        }),
    ))
}

fn runtime_phase_name(phase: RuntimePhase) -> &'static str {
    match phase {
        RuntimePhase::Idle => "idle",
        RuntimePhase::Preparing => "preparing",
        RuntimePhase::Running => "running",
        RuntimePhase::WaitingApproval => "waiting_approval",
        RuntimePhase::WaitingUser => "waiting_user",
        RuntimePhase::Cancelling => "cancelling",
        RuntimePhase::Finalizing => "finalizing",
    }
}

fn normalize_token_usage_range(value: Option<&str>) -> String {
    match value.unwrap_or("day") {
        "week" => "week".to_string(),
        "all" => "all".to_string(),
        _ => "day".to_string(),
    }
}

pub(super) fn token_usage_since(range: &str) -> Option<String> {
    let now = chrono::Utc::now();
    match range {
        "week" => Some((now - chrono::Duration::days(7)).to_rfc3339()),
        "all" => None,
        _ => Some(token_usage_today_start()),
    }
}

fn token_usage_today_start() -> String {
    let now = chrono::Local::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|start| start.and_local_timezone(chrono::Local).earliest())
        .unwrap_or(now)
        .with_timezone(&chrono::Utc);
    start.to_rfc3339()
}

fn runtime_token_usage_totals(items: &[RuntimeTokenUsage]) -> RuntimeTokenUsageBreakdown {
    items
        .iter()
        .fold(RuntimeTokenUsageBreakdown::default(), |mut total, item| {
            total.input_tokens = total.input_tokens.saturating_add(item.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(item.output_tokens);
            total.cache_creation_input_tokens = total
                .cache_creation_input_tokens
                .saturating_add(item.cache_creation_input_tokens);
            total.cache_read_input_tokens = total
                .cache_read_input_tokens
                .saturating_add(item.cache_read_input_tokens);
            total.reasoning_tokens = total.reasoning_tokens.saturating_add(item.reasoning_tokens);
            total.server_tool_tokens = total
                .server_tool_tokens
                .saturating_add(item.server_tool_tokens);
            total.total_tokens = total.total_tokens.saturating_add(item.total_tokens);
            total
        })
}

fn runtime_token_usage_by_source(
    items: &[RuntimeTokenUsage],
) -> Vec<RuntimeTokenUsageSourceSummary> {
    let mut map = BTreeMap::<String, (usize, u64)>::new();
    for item in items {
        let entry = map.entry(item.source.as_str().to_string()).or_default();
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(item.total_tokens);
    }
    map.into_iter()
        .map(
            |(source, (records, total_tokens))| RuntimeTokenUsageSourceSummary {
                source,
                records,
                total_tokens,
            },
        )
        .collect()
}

fn runtime_token_usage_by_model(items: &[RuntimeTokenUsage]) -> Vec<RuntimeTokenUsageModelSummary> {
    let mut map = BTreeMap::<(String, String), (usize, u64)>::new();
    for item in items {
        let entry = map
            .entry((item.provider.clone(), item.model.clone()))
            .or_default();
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(item.total_tokens);
    }
    map.into_iter()
        .map(
            |((provider, model), (records, total_tokens))| RuntimeTokenUsageModelSummary {
                provider,
                model,
                records,
                total_tokens,
            },
        )
        .collect()
}

/// 更新当前运行模式。
pub(crate) async fn handle_runtime_mode_update(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RuntimeModeUpdateRequest>,
) -> Result<Json<RuntimeModeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let next = parse_runtime_mode_state(&req.mode, req.focus_phase.as_deref())
        .map_err(|err| bad_request(&err))?;
    let updated = set_runtime_mode_state(&state, next).map_err(|err| bad_request(&err))?;
    Ok(Json(runtime_mode_response(updated)))
}

/// 更新运行底座工作区权限策略。
pub(crate) async fn handle_runtime_workspace_policy_update(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<RuntimeWorkspacePolicyRequest>,
) -> Result<Json<RuntimeWorkspacesResponse>, (StatusCode, Json<ErrorResponse>)> {
    let permission_mode = normalize_runtime_permission_mode(&req.permission_mode)
        .ok_or_else(|| runtime_workspace_error_response("未知权限模式。".to_string()))?;
    let sandbox_mode = normalize_runtime_sandbox_mode(&req.sandbox_mode)
        .ok_or_else(|| runtime_workspace_error_response("未知沙箱模式。".to_string()))?;
    if permission_mode == "full_access" || sandbox_mode == "danger_full_access" {
        return Err(runtime_workspace_error_response(
            "YOLO 不能保存为全局默认，请在当前会话输入区临时开启。".to_string(),
        ));
    }
    let mut config = load_runtime_harness_config();
    config.permission_mode = permission_mode;
    config.sandbox_mode = sandbox_mode;
    // 旧版曾维护“文件工具允许目录”白名单。Codex 式运行底座不再暴露或使用该白名单，
    // 保存权限模式时顺手清空历史配置，避免旧目录继续影响运行边界。
    config.roots.clear();
    save_runtime_harness_config(&config)
        .await
        .map_err(runtime_workspace_error_response)?;
    // 设置页只维护新会话默认值；当前会话始终由输入区会话级选择器控制。
    runtime_workspaces_response()
        .map(Json)
        .map_err(runtime_workspace_error_response)
}

async fn resolve_runtime_approval(
    state: Arc<AppState>,
    turn_id: String,
    approval_id: String,
    approved: bool,
    reason: Option<String>,
) -> Result<Json<ApprovalDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    resolve_runtime_approval_command(state, turn_id, approval_id, approved, reason)
        .await
        .map(Json)
}

/// 按统一运行时命令恢复等待中的工具审批。
pub(super) async fn resolve_runtime_approval_command(
    state: Arc<AppState>,
    turn_id: String,
    approval_id: String,
    approved: bool,
    reason: Option<String>,
) -> Result<ApprovalDecisionResponse, (StatusCode, Json<ErrorResponse>)> {
    if turn_id.trim().is_empty() {
        return Err(bad_request("turn_id 不能为空。"));
    }
    let reason = reason
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let resolution = state
        .runtime_service
        .resolve_approval(
            &approval_id,
            &turn_id,
            ApprovalDecision {
                approved,
                reason: reason.clone(),
            },
        )
        .await
        .map_err(|error| approval_resolution_error(&approval_id, error))?;
    if !resolution.idempotent {
        tracing::info!(
            target: "agent_vp::runtime",
            approval_id = %approval_id,
            tool_name = resolution.tool_name.as_deref().unwrap_or("unknown"),
            risk = resolution.risk.as_deref().unwrap_or("unknown"),
            approved,
            "工具审批已处理；审批说明与工具正文不写入运行日志"
        );
    }
    Ok(ApprovalDecisionResponse {
        approval_id,
        approved,
        status: "ok".to_string(),
        reason,
    })
}

async fn resolve_runtime_user_question(
    state: Arc<AppState>,
    turn_id: String,
    request_id: String,
    answered: bool,
    answers: Option<serde_json::Value>,
    annotations: Option<serde_json::Value>,
    reason: Option<String>,
) -> Result<Json<UserQuestionDecisionResponse>, (StatusCode, Json<ErrorResponse>)> {
    if turn_id.trim().is_empty() {
        return Err(bad_request("turn_id 不能为空。"));
    }
    let reason = reason
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let resolution = state
        .runtime_service
        .resolve_user_question(
            &request_id,
            &turn_id,
            UserQuestionDecision {
                answered,
                answers,
                annotations,
                reason: reason.clone(),
            },
        )
        .await
        .map_err(|error| user_question_resolution_error(&request_id, error))?;
    if !resolution.idempotent {
        tracing::info!(
            target: "agent_vp::runtime",
            request_id = %request_id,
            tool_name = resolution.tool_name.as_deref().unwrap_or("unknown"),
            answered,
            "用户问题已处理；问题正文、答案与处理说明不写入运行日志"
        );
    }
    Ok(Json(UserQuestionDecisionResponse {
        request_id,
        answered,
        status: "ok".to_string(),
        reason,
    }))
}

fn approval_resolution_error(
    approval_id: &str,
    error: InteractionResolveError,
) -> (StatusCode, Json<ErrorResponse>) {
    match error {
        InteractionResolveError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("审批 `{approval_id}` 不存在或已过期。"),
            }),
        ),
        InteractionResolveError::TurnMismatch => decision_conflict(format!(
            "审批 `{approval_id}` 属于其他回合，已拒绝过期决策。"
        )),
        InteractionResolveError::RuntimeNotWaiting => {
            decision_conflict(format!("审批 `{approval_id}` 对应的回合已不再等待审批。"))
        }
        InteractionResolveError::ConflictingDecision => {
            decision_conflict(format!("审批 `{approval_id}` 已以不同结果处理。"))
        }
        InteractionResolveError::ReceiverClosed => {
            decision_conflict(format!("审批 `{approval_id}` 对应的回合已不再等待。"))
        }
    }
}

fn user_question_resolution_error(
    request_id: &str,
    error: InteractionResolveError,
) -> (StatusCode, Json<ErrorResponse>) {
    match error {
        InteractionResolveError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("用户问题 `{request_id}` 不存在或已处理。"),
            }),
        ),
        InteractionResolveError::TurnMismatch => decision_conflict(format!(
            "用户问题 `{request_id}` 属于其他回合，已拒绝过期答案。"
        )),
        InteractionResolveError::RuntimeNotWaiting => decision_conflict(format!(
            "用户问题 `{request_id}` 对应的回合已不再等待回答。"
        )),
        InteractionResolveError::ConflictingDecision => {
            decision_conflict(format!("用户问题 `{request_id}` 已以不同结果处理。"))
        }
        InteractionResolveError::ReceiverClosed => {
            decision_conflict(format!("用户问题 `{request_id}` 对应的回合已不再等待。"))
        }
    }
}

pub(super) fn decision_conflict(message: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::CONFLICT, Json(ErrorResponse { error: message }))
}
