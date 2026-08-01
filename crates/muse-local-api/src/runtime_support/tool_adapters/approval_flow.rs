//! 工具审批等待、自动审查与结果持久化流程。

use super::*;

pub(in crate::runtime_support) struct ToolApprovalRequest<'a> {
    pub(in crate::runtime_support) tx: Option<&'a RuntimeSseSender>,
    pub(in crate::runtime_support) turn: &'a TurnContext,
    pub(in crate::runtime_support) call: &'a ToolCall,
    pub(in crate::runtime_support) risk: &'a str,
    pub(in crate::runtime_support) summary: String,
    pub(in crate::runtime_support) cancel_token: &'a RuntimeTurnCancel,
    pub(in crate::runtime_support) provider:
        &'a Arc<dyn muse_core::model::provider::ChatModelProvider>,
    pub(in crate::runtime_support) conversation: &'a Conversation,
    pub(in crate::runtime_support) approvals_reviewer: ApprovalsReviewer,
    pub(in crate::runtime_support) policy_revision: u64,
}

#[derive(Debug, Clone)]
pub(in crate::runtime_support) struct ToolApprovalEvidence {
    pub(in crate::runtime_support) approval_id: String,
    pub(in crate::runtime_support) call_id: String,
    pub(in crate::runtime_support) approved_at: String,
    pub(in crate::runtime_support) expires_at: String,
}

pub(in crate::runtime_support) async fn wait_for_tool_approval(
    state: &Arc<AppState>,
    request: ToolApprovalRequest<'_>,
) -> Result<(bool, String, Option<ToolApprovalEvidence>), ()> {
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
        return Ok((false, "no_event_channel".to_string(), None));
    };
    let memory_call_receipt = memory_session_call_receipt(&call.name, &call.arguments);
    let memory_call_receipt_json = memory_call_receipt
        .as_ref()
        .map(MemorySessionCallReceipt::to_json);
    let approval_call_id = memory_call_receipt.as_ref().map_or_else(
        || call.call_id.clone(),
        |_| memory_session_call_id(&call.call_id),
    );
    let summary = memory_call_receipt
        .as_ref()
        .map_or(summary, |receipt| receipt.approval_summary().to_string());
    // intrinsic 记忆 Tool 不进入自动审查器；memory_delete 的专用用户确认
    // 不能由模型审查结果替代，其他记忆工具若未来要求审批也按同一安全上限处理。
    let approvals_reviewer = if memory_call_receipt.is_some() {
        ApprovalsReviewer::User
    } else {
        approvals_reviewer
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
                "call_id": approval_call_id,
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
                "call_id": approval_call_id,
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
                    "call_id": approval_call_id,
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
                canonical_approval_resolved_event(
                    &approval_id,
                    call,
                    &approval_call_id,
                    memory_call_receipt_json.as_ref(),
                    true,
                    "auto_review_allowed",
                ),
            )
            .await?;
            let evidence = approval_evidence(&approval_id, &call.call_id);
            return Ok((true, "auto_review_allowed".to_string(), Some(evidence)));
        }
        if matches!(
            outcome.failure_reason(),
            Some("cancelled" | "client_disconnected")
        ) {
            return Ok((
                false,
                outcome.failure_reason().unwrap_or("cancelled").to_string(),
                None,
            ));
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
    let approval_pending_payload = with_memory_approval_receipt(
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "approval_id": approval_id,
            "call_id": approval_call_id,
            "tool": call.name,
            "risk": risk,
            "reviewer": if approvals_reviewer == ApprovalsReviewer::AutoReview { "auto_review_fallback" } else { "user" },
            "audit_note": "工具审批请求已登记；命令、查询、参数和说明未写入审计事件。",
        }),
        memory_call_receipt_json.as_ref(),
    );
    append_transcript_record(state, "approval_pending", approval_pending_payload)
        .await
        .map_err(|err| {
            tracing::error!(target: "muse::transcript", error = %err, "持久化审批请求失败");
        })?;
    emit_json_event(
        tx,
        runtime_approval_pending_event(
            &approval_id,
            &approval_call_id,
            &call.name,
            risk,
            if approvals_reviewer == ApprovalsReviewer::AutoReview {
                "AUTO 审查未放行，需要你人工确认后才能继续。"
            } else {
                "这个工具需要你的确认后才能继续。"
            },
            Some(&manual_summary),
            memory_call_receipt_json.as_ref().unwrap_or(&call.arguments),
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
    let approval_resolved_payload = with_memory_approval_receipt(
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "approval_id": approval_id,
            "call_id": approval_call_id,
            "tool": call.name,
            "approved": approved,
            "audit_note": "工具审批结果已登记；用户说明未写入审计事件。",
        }),
        memory_call_receipt_json.as_ref(),
    );
    append_transcript_record(state, "approval_resolved", approval_resolved_payload)
        .await
        .map_err(|err| {
            tracing::error!(target: "muse::transcript", error = %err, "持久化审批结果失败");
        })?;
    if !tx.is_closed() {
        let _ = emit_json_event(
            tx,
            canonical_approval_resolved_event(
                &approval_id,
                call,
                &approval_call_id,
                memory_call_receipt_json.as_ref(),
                approved,
                &reason,
            ),
        )
        .await;
    }
    let evidence = approved.then(|| approval_evidence(&approval_id, &call.call_id));
    Ok((approved, reason, evidence))
}

fn approval_evidence(approval_id: &str, call_id: &str) -> ToolApprovalEvidence {
    let approved_at = chrono::Utc::now();
    let expires_at = approved_at + chrono::Duration::seconds(TOOL_APPROVAL_TIMEOUT_SECS as i64);
    ToolApprovalEvidence {
        approval_id: approval_id.to_string(),
        call_id: call_id.to_string(),
        approved_at: approved_at.to_rfc3339(),
        expires_at: expires_at.to_rfc3339(),
    }
}

fn with_memory_approval_receipt(
    mut payload: serde_json::Value,
    receipt: Option<&serde_json::Value>,
) -> serde_json::Value {
    if let (Some(object), Some(receipt)) = (payload.as_object_mut(), receipt) {
        object.insert("canonical_arguments".to_string(), receipt.clone());
    }
    payload
}

fn canonical_approval_resolved_event(
    approval_id: &str,
    call: &ToolCall,
    canonical_call_id: &str,
    memory_receipt: Option<&serde_json::Value>,
    approved: bool,
    reason: &str,
) -> serde_json::Value {
    let Some(memory_receipt) = memory_receipt else {
        return runtime_approval_resolved_event(approval_id, approved, Some(reason));
    };
    let reason = match reason {
        "timeout" => "timeout",
        "turn_cancelled" => "turn_cancelled",
        "client_disconnected" => "client_disconnected",
        "approval_channel_dropped" => "approval_channel_dropped",
        "auto_review_allowed" => "auto_review_allowed",
        _ if approved => "approved",
        _ => "rejected",
    };
    serde_json::json!({
        "type": "approval_resolved",
        "phase": "approval_resolved",
        "approval_id": approval_id,
        "call_id": canonical_call_id,
        "name": call.name,
        "approved": approved,
        "reason": reason,
        "arguments": memory_receipt,
        "state": "completed",
    })
}
