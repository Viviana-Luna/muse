struct ToolApprovalRequest<'a> {
    tx: Option<&'a RuntimeSseSender>,
    turn: &'a TurnContext,
    call: &'a ToolCall,
    risk: &'a str,
    summary: String,
    cancel_token: &'a RuntimeTurnCancel,
    provider: &'a Arc<dyn muse_core::model::provider::ChatModelProvider>,
    conversation: &'a Conversation,
    approvals_reviewer: ApprovalsReviewer,
    policy_revision: u64,
}

async fn wait_for_tool_approval(
    state: &Arc<AppState>,
    request: ToolApprovalRequest<'_>,
) -> Result<(bool, String), ()> {
    let ToolApprovalRequest {
        tx,
        turn,
        call,
        risk,
        summary,
        cancel_token,
        provider,
        conversation,
        approvals_reviewer,
        policy_revision,
    } = request;
    let Some(tx) = tx else {
        return Ok((false, "no_event_channel".to_string()));
    };
    let approval_id = next_runtime_id("approval");
    state
        .runtime_service
        .transition_active(&turn.turn_id, RuntimePhase::WaitingApproval)
        .map_err(|_| ())?;
    let mut manual_summary = summary.clone();
    if approvals_reviewer == ApprovalsReviewer::AutoReview {
        let started_at = Instant::now();
        append_transcript_record(
            state,
            "approval_review_started",
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "approval_id": approval_id,
                "call_id": call.call_id,
                "tool": call.name,
                "risk": risk,
                "policy_revision": policy_revision,
                "audit_note": "独立审查会话已启动；工具参数和用户正文未写入审计事件。",
            }),
        )
        .await
        .map_err(|_| ())?;
        emit_json_event(
            tx,
            approval_review_started_event(&approval_id, call, risk, policy_revision),
        )
        .await?;
        let outcome = review_tool_approval(
            provider,
            conversation,
            call,
            risk,
            &summary,
            cancel_token,
            tx,
        )
        .await;
        let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        append_transcript_record(
            state,
            outcome.event_kind(),
            serde_json::json!({
                "conversation_id": turn.conversation_id,
                "turn_id": turn.turn_id,
                "approval_id": approval_id,
                "call_id": call.call_id,
                "tool": call.name,
                "allowed": outcome.allowed(),
                "risk_level": outcome.risk_level(),
                "user_authorization": outcome.user_authorization(),
                "rationale": outcome.rationale(),
                "elapsed_ms": elapsed_ms,
                "policy_revision": policy_revision,
                "audit_note": "自动审查结果已登记；工具参数和用户正文未写入审计事件。",
            }),
        )
        .await
        .map_err(|_| ())?;
        emit_json_event(
            tx,
            approval_review_outcome_event(
                &approval_id,
                call,
                &outcome,
                elapsed_ms,
                policy_revision,
            ),
        )
        .await?;
        let circuit_opened = state
            .runtime_service
            .record_approval_review_outcome(outcome.allowed())
            .map_err(|_| ())?;
        if circuit_opened {
            let fallback_revision = match state.runtime_service.session_repository().await {
                Ok(repository) => match repository
                    .update_approval_mode(&turn.conversation_id, ApprovalModePreset::Manual)
                    .await
                {
                    Ok(saved) => {
                        let _ = set_active_approval_mode(state, saved.preset, saved.revision);
                        saved.revision
                    }
                    Err(error) => {
                        tracing::warn!(%error, "持久化 AUTO 断路器回退失败，继续保持内存手动审批");
                        state
                            .runtime_service
                            .execution_policy()
                            .map(|policy| policy.revision)
                            .unwrap_or(policy_revision)
                    }
                },
                Err(error) => {
                    tracing::warn!(%error, "打开会话仓储失败，AUTO 断路器只保留内存手动审批");
                    state
                        .runtime_service
                        .execution_policy()
                        .map(|policy| policy.revision)
                        .unwrap_or(policy_revision)
                }
            };
            let _ = emit_json_event(
                tx,
                runtime_event_payload(RuntimeEvent::ApprovalModeChanged {
                    preset: ApprovalModePreset::Manual.as_str().to_string(),
                    revision: fallback_revision,
                    reason: "连续三次自动审查未放行，已回退为手动审批。".to_string(),
                }),
            )
            .await;
        }
        if outcome.allowed() {
            if state.runtime_service.snapshot().is_ok_and(|snapshot| {
                snapshot.turn_id.as_deref() == Some(turn.turn_id.as_str())
                    && snapshot.phase == RuntimePhase::WaitingApproval
            }) {
                let _ = state
                    .runtime_service
                    .transition_active(&turn.turn_id, RuntimePhase::Running);
            }
            append_transcript_record(
                state,
                "approval_resolved",
                serde_json::json!({
                    "conversation_id": turn.conversation_id,
                    "turn_id": turn.turn_id,
                    "approval_id": approval_id,
                    "call_id": call.call_id,
                    "tool": call.name,
                    "approved": true,
                    "reviewer": "auto_review",
                    "audit_note": "工具由独立审查器允许；理由已记录在审查事件中。",
                }),
            )
            .await
            .map_err(|_| ())?;
            emit_json_event(
                tx,
                runtime_approval_resolved_event(
                    &approval_id,
                    true,
                    Some("auto_review_allowed"),
                ),
            )
            .await?;
            return Ok((true, "auto_review_allowed".to_string()));
        }
        if matches!(
            outcome.failure_reason(),
            Some("cancelled" | "client_disconnected")
        ) {
            return Ok((false, outcome.failure_reason().unwrap_or("cancelled").to_string()));
        }
        manual_summary = format!(
            "AUTO 审查未放行：{}\n你可以仅针对下面这一项动作进行人工确认。\n{}",
            outcome.rationale(),
            summary
        );
        if circuit_opened {
            manual_summary.push_str("\n连续三次自动审查未放行，当前会话已收紧为手动审批。");
        }
    }

    let (approval_tx, approval_rx) = oneshot::channel::<ApprovalDecision>();
    state
        .runtime_service
        .register_pending_approval(
            approval_id.clone(),
            PendingApproval {
                turn_id: turn.turn_id.clone(),
                tool_name: call.name.clone(),
                risk: risk.to_string(),
                summary: manual_summary.clone(),
                tx: approval_tx,
            },
        )
        .await
        .map_err(|_| ())?;
    let _pending_guard = PendingInteractionGuard::approval(state, &turn.turn_id, &approval_id);
    append_transcript_record(
        state,
        "approval_pending",
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "approval_id": approval_id,
            "call_id": call.call_id,
            "tool": call.name,
            "risk": risk,
            "reviewer": if approvals_reviewer == ApprovalsReviewer::AutoReview { "auto_review_fallback" } else { "user" },
            "audit_note": "工具审批请求已登记；命令、查询、参数和说明未写入审计事件。",
        }),
    )
    .await
    .map_err(|err| {
        tracing::error!(target: "muse::transcript", error = %err, "持久化审批请求失败");
    })?;
    emit_json_event(
        tx,
        runtime_approval_pending_event(
            &approval_id,
            &call.call_id,
            &call.name,
            risk,
            if approvals_reviewer == ApprovalsReviewer::AutoReview {
                "AUTO 审查未放行，需要你人工确认后才能继续。"
            } else {
                "这个工具需要你的确认后才能继续。"
            },
            Some(&manual_summary),
            &call.arguments,
        ),
    )
    .await?;
    let (approved, reason) = tokio::select! {
        result = approval_rx => match result {
            Ok(decision) => {
                let reason = if decision.approved {
                    decision.reason.unwrap_or_else(|| "approved".to_string())
                } else {
                    decision.reason.unwrap_or_else(|| "rejected".to_string())
                };
                (decision.approved, reason)
            }
            Err(_) => (false, "approval_channel_dropped".to_string()),
        },
        _ = tokio::time::sleep(Duration::from_secs(TOOL_APPROVAL_TIMEOUT_SECS)) => {
            state.runtime_service.remove_pending_approval(&approval_id, &turn.turn_id).await;
            (false, "timeout".to_string())
        },
        _ = wait_for_turn_cancel(cancel_token) => {
            state.runtime_service.remove_pending_approval(&approval_id, &turn.turn_id).await;
            (false, "turn_cancelled".to_string())
        },
        _ = tx.closed() => {
            state.runtime_service.remove_pending_approval(&approval_id, &turn.turn_id).await;
            (false, "client_disconnected".to_string())
        },
    };
    if state.runtime_service.snapshot().is_ok_and(|snapshot| {
        snapshot.turn_id.as_deref() == Some(turn.turn_id.as_str())
            && snapshot.phase == RuntimePhase::WaitingApproval
    }) {
        let _ = state
            .runtime_service
            .transition_active(&turn.turn_id, RuntimePhase::Running);
    }
    append_transcript_record(
        state,
        "approval_resolved",
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "approval_id": approval_id,
            "call_id": call.call_id,
            "tool": call.name,
            "approved": approved,
            "audit_note": "工具审批结果已登记；用户说明未写入审计事件。",
        }),
    )
    .await
    .map_err(|err| {
        tracing::error!(target: "muse::transcript", error = %err, "持久化审批结果失败");
    })?;
    if !tx.is_closed() {
        let _ = emit_json_event(
            tx,
            runtime_approval_resolved_event(&approval_id, approved, Some(&reason)),
        )
        .await;
    }
    Ok((approved, reason))
}
