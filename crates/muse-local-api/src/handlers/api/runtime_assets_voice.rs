/// 语音能力探测：前端据此决定语音按钮是否可用。
pub(crate) async fn handle_voice_capabilities(
    State(state): State<Arc<AppState>>,
) -> Json<VoiceCapabilitiesResponse> {
    let tts_config = {
        let config = state.model_config.lock().await;
        config.tts().clone()
    };
    let tts_available =
        tts_config.is_external_provider() && effective_tts_config(&tts_config).enabled();
    Json(VoiceCapabilitiesResponse {
        tts: tts_available,
        speech_recognition: state.speech_recognition_provider.lock().await.is_some(),
    })
}

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
    let policy = FrozenExecutionPolicy::from_preset(
        saved.preset,
        current.allowed_roots,
        saved.revision,
    );
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

/// 执行设置中心连通性诊断。
pub(crate) async fn handle_diagnostics_connectivity(
    State(state): State<Arc<AppState>>,
) -> Json<DiagnosticsConnectivityResponse> {
    let (chat_base, tts_base, asr_base) = {
        let store = state.model_config.lock().await;
        (
            store.chat().api_base.clone(),
            store.tts().api_base.clone(),
            store.speech_recognition().api_base.clone(),
        )
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build();

    let mut checks = Vec::new();
    match client {
        Ok(client) => {
            checks.push(probe_http_connectivity(&client, "chat", "对话模型 API", chat_base).await);
            checks.push(probe_http_connectivity(&client, "tts", "TTS API", tts_base).await);
            checks.push(probe_http_connectivity(&client, "asr", "语音识别 API", asr_base).await);
        }
        Err(err) => {
            checks.push(diagnostics_failed_item(
                "chat",
                "对话模型 API",
                chat_base,
                format!("创建诊断客户端失败：{err}"),
            ));
            checks.push(diagnostics_failed_item(
                "tts",
                "TTS API",
                tts_base,
                format!("创建诊断客户端失败：{err}"),
            ));
            checks.push(diagnostics_failed_item(
                "asr",
                "语音识别 API",
                asr_base,
                format!("创建诊断客户端失败：{err}"),
            ));
        }
    }

    Json(DiagnosticsConnectivityResponse { checks })
}

async fn probe_http_connectivity(
    client: &reqwest::Client,
    id: &str,
    label: &str,
    target: String,
) -> DiagnosticsConnectivityItem {
    let target = target.trim().trim_end_matches('/').to_string();
    if target.is_empty() {
        return DiagnosticsConnectivityItem {
            id: id.to_string(),
            label: label.to_string(),
            target: "未配置".to_string(),
            status: "skipped".to_string(),
            latency_ms: None,
            message: "未配置 API Base，跳过检测。".to_string(),
        };
    }

    let started_at = Instant::now();
    match client.get(&target).send().await {
        Ok(response) => DiagnosticsConnectivityItem {
            id: id.to_string(),
            label: label.to_string(),
            target,
            status: "reachable".to_string(),
            latency_ms: Some(started_at.elapsed().as_millis() as u64),
            message: format!("服务已响应：HTTP {}。", response.status()),
        },
        Err(err) => DiagnosticsConnectivityItem {
            id: id.to_string(),
            label: label.to_string(),
            target,
            status: "failed".to_string(),
            latency_ms: Some(started_at.elapsed().as_millis() as u64),
            message: format!("连接失败：{err}"),
        },
    }
}

fn diagnostics_failed_item(
    id: &str,
    label: &str,
    target: String,
    message: String,
) -> DiagnosticsConnectivityItem {
    DiagnosticsConnectivityItem {
        id: id.to_string(),
        label: label.to_string(),
        target: if target.trim().is_empty() {
            "未配置".to_string()
        } else {
            target
        },
        status: "failed".to_string(),
        latency_ms: None,
        message,
    }
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

fn token_usage_since(range: &str) -> Option<String> {
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
async fn resolve_runtime_approval_command(
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

fn decision_conflict(message: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::CONFLICT, Json(ErrorResponse { error: message }))
}

/// 上传角色图片资源，并返回可由前端直接引用的本地资源 URL。
pub(crate) async fn handle_upload_asset(
    mut multipart: Multipart,
) -> Result<Json<AssetUploadResponse>, (StatusCode, Json<ErrorResponse>)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| bad_request(&format!("读取上传表单失败：{err}")))?
    {
        if field.name() != Some("file") {
            continue;
        }

        let content_type = field.content_type().map(str::to_string);
        let declared_extension = uploaded_image_extension(content_type.as_deref());
        let bytes = field
            .bytes()
            .await
            .map_err(|err| bad_request(&format!("读取上传文件失败：{err}")))?;
        if bytes.is_empty() {
            return Err(bad_request("上传图片不能为空。"));
        }
        if bytes.len() > MAX_PERSONA_IMAGE_BYTES {
            return Err(bad_request("上传图片不能超过 5MB。"));
        }
        let extension = uploaded_image_extension_from_bytes(&bytes)
            .ok_or_else(|| bad_request("角色图片内容不是有效的 PNG、JPEG 或 WebP 格式。"))?;
        if declared_extension.is_some_and(|declared| declared != extension) {
            return Err(bad_request("上传图片声明的类型与文件内容不一致。"));
        }

        let filename = format!("{}.{}", content_hash_hex(&bytes), extension);
        let dir = uploaded_assets_dir();
        fs::create_dir_all(&dir)
            .await
            .map_err(|err| internal_error(format!("创建角色图片目录失败：{err}")))?;
        let path = dir.join(&filename);
        persist_uploaded_asset(&path, &bytes)
            .await
            .map_err(internal_error)?;

        return Ok(Json(AssetUploadResponse {
            url: format!("/api/assets/uploaded/{filename}"),
        }));
    }

    Err(bad_request("请在 `file` 字段中上传角色图片。"))
}

/// 经鉴权读取单个上传图片；不暴露目录浏览，也不跟随符号链接。
pub(crate) async fn handle_uploaded_asset(
    Path(filename): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let (expected_hash, extension) = validated_uploaded_asset_name(&filename)
        .ok_or_else(|| bad_request("上传资源文件名无效。"))?;
    let path = uploaded_assets_dir().join(&filename);
    let bytes = match tokio::task::spawn_blocking(move || read_uploaded_asset_file(&path)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "上传资源不存在。".to_string(),
                }),
            ));
        }
        Ok(Err(error)) => return Err(internal_error(format!("读取上传资源失败：{error}"))),
        Err(error) => return Err(internal_error(format!("上传资源读取任务异常结束：{error}"))),
    };
    let detected_extension = uploaded_image_extension_from_bytes(&bytes);
    if detected_extension != Some(extension) || content_hash_hex(&bytes) != expected_hash {
        return Err(internal_error("上传资源完整性校验失败。".to_string()));
    }
    let content_type = match extension {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => return Err(internal_error("上传资源类型无效。".to_string())),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header(header::ETAG, format!("\"{expected_hash}\""))
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|error| internal_error(format!("构造上传资源响应失败：{error}")))
}

/// 回滚尚未被任何角色展示包引用的上传图片。
///
/// 内容哈希文件可能已经被其他角色复用，因此“仍被引用”和“文件已不存在”都视为
/// 回滚目标已经满足；只有确认无引用时才删除磁盘文件。
pub(crate) async fn handle_discard_uploaded_asset(
    Path(filename): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    validated_uploaded_asset_name(&filename)
        .ok_or_else(|| bad_request("上传资源文件名无效。"))?;
    let url = format!("/api/assets/uploaded/{filename}");
    discard_unreferenced_uploaded_asset(&state, &url)
        .await
        .map_err(|error| internal_error(format!("回滚无引用上传资源失败：{error}")))?;
    Ok(StatusCode::NO_CONTENT)
}

fn read_uploaded_asset_file(path: &StdPath) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "上传资源不是普通文件。",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "上传资源不能是 Windows reparse point。",
            ));
        }
    }
    if metadata.len() == 0 || metadata.len() > MAX_PERSONA_IMAGE_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "上传资源大小超出安全范围。",
        ));
    }
    let mut reader = std::io::Read::take(file, MAX_PERSONA_IMAGE_BYTES as u64 + 1);
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::read_to_end(&mut reader, &mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_PERSONA_IMAGE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "上传资源读取过程中超出安全范围。",
        ));
    }
    Ok(bytes)
}

/// 调用已配置的 OpenAI-compatible 语音合成服务。
pub(crate) async fn handle_tts_runtime_route(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TtsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    require_active_persona_http(&state).await?;
    let text = req.text.trim();
    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "合成文本不能为空。".to_string(),
            }),
        ));
    }

    tracing::info!(
        target: "agent_vp::voice",
        action = "tts_runtime_route",
        voice_id = req.voice_id.as_deref().unwrap_or("active"),
        text_chars = text.chars().count(),
        "收到语音合成请求"
    );
    let context = resolve_tts_request_context(&state, req.voice_id.as_deref()).await?;
    let provider = if context.effective_tts.is_external_provider() {
        muse_core::speech::factory::create_tts_provider(&context.effective_tts)
            .map_err(voice_error_response)?
            .ok_or_else(|| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse {
                        error: "外接 TTS 服务未启用，请先在设置页配置语音服务。".to_string(),
                    }),
                )
            })?
    } else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "请先配置 OpenAI-compatible 语音合成服务。".to_string(),
            }),
        ));
    };
    let audio = provider
        .synthesize(text)
        .await
        .map_err(voice_error_response)?;

    tracing::info!(
        target: "agent_vp::voice",
        action = "tts_runtime_route",
        content_type = %audio.content_type,
        audio_bytes = audio.bytes.len(),
        "语音合成完成"
    );
    let mut response = Response::new(Body::from(audio.bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&audio.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Ok(response)
}

/// 语音转文本：接收 multipart 音频文件并调用远程转写服务。
///
/// 识别服务未启用时返回 503。表单字段 `audio` 为音频二进制，
/// 可选 `format` 字段为 MIME（默认 audio/webm）。
pub(crate) async fn handle_speech_transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<SpeechTranscriptionResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_active_persona_http(&state).await?;
    let provider = match state.speech_recognition_provider.lock().await.clone() {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "语音识别服务未启用，请先配置 OpenAI-compatible 转写服务。"
                        .to_string(),
                }),
            ));
        }
    };

    let (audio, audio_format, _) = parse_transcribe_audio_multipart(&mut multipart).await?;
    tracing::info!(
        target: "agent_vp::voice",
        action = "speech_transcribe",
        audio_format = %audio_format,
        audio_bytes = audio.len(),
        "收到语音转文本请求"
    );

    let text = provider
        .transcribe(audio, &audio_format)
        .await
        .map_err(voice_error_response)?;
    tracing::info!(
        target: "agent_vp::voice",
        action = "speech_transcribe",
        text_chars = text.chars().count(),
        "语音转文本完成"
    );

    Ok(Json(SpeechTranscriptionResponse { text }))
}

async fn parse_transcribe_audio_multipart(
    multipart: &mut axum::extract::Multipart,
) -> Result<(Vec<u8>, String, String), (StatusCode, Json<ErrorResponse>)> {
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut declared_content_type = None::<String>;
    let mut requested_format = None::<String>;
    let mut filename = "recording.wav".to_string();

    loop {
        let Some(field) = multipart
            .next_field()
            .await
            .map_err(|err| bad_request(&format!("读取语音上传表单失败：{err}")))?
        else {
            break;
        };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "audio" => {
                if let Some(ct) = field.content_type() {
                    declared_content_type = Some(ct.to_string());
                }
                if let Some(upload_name) = field.file_name() {
                    let trimmed = upload_name.trim();
                    if !trimmed.is_empty() {
                        filename = trimmed.to_string();
                    }
                }
                audio_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|err| {
                            (
                                StatusCode::BAD_REQUEST,
                                Json(ErrorResponse {
                                    error: format!("读取音频字段失败：{err}"),
                                }),
                            )
                        })?
                        .to_vec(),
                );
            }
            "format" => {
                let value = field
                    .text()
                    .await
                    .map_err(|err| {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(ErrorResponse {
                                error: format!("读取 format 字段失败：{err}"),
                            }),
                        )
                    })?
                    .trim()
                    .to_string();
                if !value.is_empty() {
                    requested_format = Some(value);
                }
            }
            _ => {}
        }
    }

    let audio = audio_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "缺少 audio 字段。".to_string(),
            }),
        )
    })?;
    let audio_format = validate_transcribe_audio(
        &audio,
        declared_content_type.as_deref(),
        requested_format.as_deref(),
    )
    .map_err(|message| bad_request(&message))?;
    Ok((audio, audio_format.to_string(), filename))
}

fn validate_transcribe_audio(
    audio: &[u8],
    declared_content_type: Option<&str>,
    requested_format: Option<&str>,
) -> Result<&'static str, String> {
    if audio.len() > MAX_SPEECH_UPLOAD_BYTES {
        return Err(format!(
            "音频文件超过 {} 字节上限。",
            MAX_SPEECH_UPLOAD_BYTES
        ));
    }
    if !is_pcm_wav_container(audio) {
        return Err("音频文件不是有效的 PCM WAV 容器。".to_string());
    }
    for (label, declared) in [
        ("Content-Type", declared_content_type),
        ("format", requested_format),
    ] {
        if let Some(value) = declared {
            let mime = value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !matches!(mime.as_str(), "audio/wav" | "audio/wave" | "audio/x-wav") {
                return Err(format!(
                    "音频真实类型为 audio/wav，但 {label} 声明为 `{value}`。"
                ));
            }
        }
    }
    Ok("audio/wav")
}

fn is_pcm_wav_container(audio: &[u8]) -> bool {
    if audio.len() < 44 || &audio[..4] != b"RIFF" || &audio[8..12] != b"WAVE" {
        return false;
    }
    let declared_size = u32::from_le_bytes([audio[4], audio[5], audio[6], audio[7]]) as usize;
    let Some(container_end) = declared_size.checked_add(8) else {
        return false;
    };
    if container_end < 12 || container_end > audio.len() {
        return false;
    }

    let mut offset = 12usize;
    let mut pcm_format = false;
    let mut nonempty_data = false;
    while offset.saturating_add(8) <= container_end {
        let chunk_id = &audio[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            audio[offset + 4],
            audio[offset + 5],
            audio[offset + 6],
            audio[offset + 7],
        ]) as usize;
        let data_start = offset + 8;
        let Some(data_end) = data_start.checked_add(chunk_size) else {
            return false;
        };
        if data_end > container_end {
            return false;
        }
        if chunk_id == b"fmt " && chunk_size >= 16 {
            let format = u16::from_le_bytes([audio[data_start], audio[data_start + 1]]);
            pcm_format = format == 1;
        } else if chunk_id == b"data" {
            nonempty_data = chunk_size > 0;
        }
        let Some(next) = data_end.checked_add(chunk_size % 2) else {
            return false;
        };
        offset = next;
    }
    pcm_format && nonempty_data
}
