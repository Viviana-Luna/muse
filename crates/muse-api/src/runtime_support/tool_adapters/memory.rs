//! intrinsic 记忆工具的运行时处理器。
//!
//! 三个处理器只承接 F1 冻结的领域契约：查询绑定冻结 TurnContext 的 persona scope
//! （模型永远不能传 persona_id）；变更只进 Turn 局部暂存并在 committed 后原子提交；
//! 删除必须走专用用户确认，普通审批、角色提示词或模型自述都不能替代。

use super::*;

use muse_core::domain::memory::{
    ConfirmedMemoryDeleteRequest, MAX_MEMORY_MUTATIONS, MEMORY_DELETE_TOOL_NAME,
    MEMORY_MUTATE_TOOL_NAME, MEMORY_QUERY_TOOL_NAME, MemoryConfirmationMode,
    MemoryDeleteConfirmation, MemoryDeleteConfirmationSource, MemoryDeleteParams, MemoryError,
    MemoryErrorCode, MemoryId, MemoryMutateRequest, MemoryMutationBatchReceipt,
    MemoryMutationBatchState, MemoryMutationItemReceipt, MemoryMutationItemState,
    MemoryMutationProposal, MemoryPersonaScope, MemoryQueryParams, MemoryRetrievalFilters,
    MemoryRetrievalRequest, MemoryRevisionId, MemoryStagedMutation,
};

pub(in crate::runtime_support) struct MemoryQueryHandler;
pub(in crate::runtime_support) struct MemoryMutateHandler;
pub(in crate::runtime_support) struct MemoryDeleteHandler;

type IndexedMemoryMutationProposal = (usize, MemoryMutationProposal);
type ParsedMemoryMutationBatch = (
    Vec<IndexedMemoryMutationProposal>,
    Vec<MemoryMutationItemReceipt>,
);

pub(in crate::runtime_support) static MEMORY_QUERY_HANDLER: MemoryQueryHandler = MemoryQueryHandler;
pub(in crate::runtime_support) static MEMORY_MUTATE_HANDLER: MemoryMutateHandler =
    MemoryMutateHandler;
pub(in crate::runtime_support) static MEMORY_DELETE_HANDLER: MemoryDeleteHandler =
    MemoryDeleteHandler;

fn memory_tool_failed(error: MemoryError) -> ToolResult {
    memory_tool_failure(error.code())
}

/// memory_mutate 专用失败形态：除稳定错误码外，显式要求模型不得声称已保存。
///
/// 模型收到失败收据后仍可能按角色习惯回复“记下了”；把“未保存”写进失败正文，
/// 让如实转述成为默认行为，而不是依赖模型自行推理。
pub(in crate::runtime_support) fn memory_mutate_failure(code: MemoryErrorCode) -> ToolResult {
    ToolResult {
        status: ToolResultStatus::Failed,
        content: format!(
            "{} 本次没有保存任何记忆，回复中不得声称已记住、已记录或已保存。",
            MemoryError::new(code)
        ),
        structured: Some(serde_json::json!({ "error_code": code })),
    }
}

/// 记忆工具专用失败包装：模型、SSE 与 Session 消费端只依赖同一个稳定字段。
pub(in crate::runtime_support) fn memory_tool_failure(code: MemoryErrorCode) -> ToolResult {
    ToolResult {
        status: ToolResultStatus::Failed,
        content: MemoryError::new(code).to_string(),
        structured: Some(serde_json::json!({ "error_code": code })),
    }
}

/// 在进入具体 Handler 前，把三个精确记忆工具名的失败收口到记忆领域协议。
/// 相似前缀或其他工具继续使用运行时的通用失败结构。
pub(in crate::runtime_support) fn memory_pre_handler_failure(
    tool_name: &str,
) -> Option<ToolResult> {
    let code = match tool_name {
        MEMORY_QUERY_TOOL_NAME => MemoryErrorCode::QueryRejected,
        MEMORY_MUTATE_TOOL_NAME | MEMORY_DELETE_TOOL_NAME => MemoryErrorCode::InvalidRequest,
        _ => return None,
    };
    Some(memory_tool_failure(code))
}

/// memory_delete 未取得专用真人确认时的唯一模型可见失败形态。
pub(in crate::runtime_support) fn memory_delete_confirmation_required() -> ToolResult {
    memory_tool_failure(MemoryErrorCode::DeleteConfirmationRequired)
}

/// 从冻结 TurnContext 解析 persona scope；无活动角色时记忆工具一律拒绝。
fn memory_scope_for_turn(
    turn: &TurnContext,
    code: MemoryErrorCode,
) -> Result<MemoryPersonaScope, ToolResult> {
    let Some(persona_id) = turn.persona_id.as_deref() else {
        return Err(memory_tool_failure(code));
    };
    MemoryPersonaScope::new(persona_id).map_err(memory_tool_failed)
}

fn check_memory_permissions(turn: &TurnContext, call: &ToolCall) -> Result<(), ToolResult> {
    if runtime_tool_allowed(turn, &call.name) {
        Ok(())
    } else {
        Err(memory_tool_failure(MemoryErrorCode::InvalidRequest))
    }
}

impl RuntimeToolHandler for MemoryQueryHandler {
    fn name(&self) -> &'static str {
        MEMORY_QUERY_TOOL_NAME
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        let params: MemoryQueryParams = serde_json::from_value(call.arguments.clone())
            .map_err(|_| memory_tool_failure(MemoryErrorCode::InvalidRequest))?;
        params.validate().map_err(memory_tool_failed)
    }

    fn check_permissions(
        &self,
        _state: &Arc<AppState>,
        turn: &TurnContext,
        call: &ToolCall,
    ) -> Result<(), ToolResult> {
        check_memory_permissions(turn, call)
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            let RuntimeToolInvocation {
                state,
                turn,
                call,
                memory_turn,
                ..
            } = invocation;
            let Some(services) = state.memory.as_ref() else {
                // 未接线是稳定拒绝而不是内部错误，模型可据此放弃记忆路径继续回答。
                return memory_tool_failure(MemoryErrorCode::QueryRejected);
            };
            let Ok(params) = serde_json::from_value::<MemoryQueryParams>(call.arguments.clone())
            else {
                return memory_tool_failure(MemoryErrorCode::InvalidRequest);
            };
            let scope = match memory_scope_for_turn(turn, MemoryErrorCode::QueryRejected) {
                Ok(scope) => scope,
                Err(result) => return result,
            };
            // Turn 查询预算按 F2 记账：耗尽的调用必须稳定失败，不能继续穿透到检索层。
            if !memory_turn.try_consume_query_budget() {
                return memory_tool_failed(MemoryError::new(MemoryErrorCode::QueryBudgetExceeded));
            }
            let retrieval_turn = match memory_turn.retrieval_turn() {
                Ok(turn) => turn,
                Err(error) => return memory_tool_failed(error),
            };
            let request = match MemoryRetrievalRequest::bind(
                params,
                scope,
                retrieval_turn,
                MemoryRetrievalFilters::none(),
            ) {
                Ok(request) => request,
                Err(error) => return memory_tool_failed(error),
            };
            let receipt = match services.retriever.retrieve(&request) {
                Ok(receipt) => receipt,
                Err(error) => return memory_tool_failed(error),
            };
            memory_turn.record_query_page(
                call.call_id.clone(),
                receipt
                    .items
                    .iter()
                    .map(|item| item.memory_id.clone())
                    .collect(),
            );
            let content = if receipt.items.is_empty() {
                "未查到相关长期记忆。".to_string()
            } else {
                format!("查到 {} 条相关长期记忆。", receipt.items.len())
            };
            ToolResult {
                status: ToolResultStatus::Success,
                content,
                structured: serde_json::to_value(&receipt).ok(),
            }
        })
    }
}

impl RuntimeToolHandler for MemoryMutateHandler {
    fn name(&self) -> &'static str {
        MEMORY_MUTATE_TOOL_NAME
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        if let Some(mutations) = call.arguments.get("mutations") {
            let Some(mutations) = mutations.as_array() else {
                return Err(memory_mutate_failure(MemoryErrorCode::InvalidRequest));
            };
            if mutations.is_empty() || mutations.len() > MAX_MEMORY_MUTATIONS {
                return Err(memory_mutate_failure(MemoryErrorCode::InvalidRequest));
            }
            return Ok(());
        }
        serde_json::from_value::<MemoryMutateRequest>(call.arguments.clone())
            .map(|_| ())
            .map_err(|_| memory_mutate_failure(MemoryErrorCode::InvalidRequest))
    }

    fn check_permissions(
        &self,
        _state: &Arc<AppState>,
        turn: &TurnContext,
        call: &ToolCall,
    ) -> Result<(), ToolResult> {
        check_memory_permissions(turn, call)
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_mutating(&self, _call: &ToolCall) -> bool {
        // 该调用只写 Turn 局部暂存，不产生外部副作用；durable 提交由 committed 之后的
        // 批量路径完成。标 mutating 会让取消中的 Turn 被误记为已产生外部效果。
        false
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            let RuntimeToolInvocation {
                state,
                tx,
                turn,
                call,
                cancel_token,
                memory_turn,
                conversation,
                ..
            } = invocation;
            let Some(services) = state.memory.as_ref() else {
                return memory_tool_failed(MemoryError::new(
                    MemoryErrorCode::RepositoryUnavailable,
                ));
            };
            let (proposals, mut receipts) = match parse_memory_mutation_proposals(&call.arguments) {
                Ok(parsed) => parsed,
                Err(result) => return result,
            };
            let scope = match memory_scope_for_turn(turn, MemoryErrorCode::SourceIneligible) {
                Ok(scope) => scope,
                Err(result) => return result,
            };
            let confirmation_questions = proposals
                .iter()
                .filter(|(_, proposal)| proposal.confirmation == MemoryConfirmationMode::AskUser)
                .map(|(index, proposal)| {
                    let question = format!(
                        "是否保存这条记忆？\n原文：{}\n摘要：{}",
                        proposal
                            .source_quote
                            .as_deref()
                            .unwrap_or("[旧格式未提供原文锚点]"),
                        proposal.content
                    );
                    (
                        *index,
                        question.clone(),
                        AskUserQuestionItem {
                            question,
                            header: format!("确认记忆 {}", index + 1),
                            options: vec![
                                AskUserQuestionOption {
                                    label: "确认保存".to_string(),
                                    description: "确认该摘要准确表达了你的原意。".to_string(),
                                },
                                AskUserQuestionOption {
                                    label: "跳过".to_string(),
                                    description: "本轮不保存这条候选记忆。".to_string(),
                                },
                            ],
                            multi_select: false,
                        },
                    )
                })
                .collect::<Vec<_>>();
            let mut confirmed = std::collections::BTreeSet::new();
            if !confirmation_questions.is_empty() {
                if let Some(tx) = tx {
                    let _ = emit_json_event(
                        tx,
                        runtime_event_payload(RuntimeEvent::MemoryConfirmationRequired {
                            turn_id: turn.turn_id.clone(),
                            call_id: call.call_id.clone(),
                            candidate_count: confirmation_questions.len(),
                        }),
                    )
                    .await;
                }
                let request = AskUserQuestionRequest {
                    questions: confirmation_questions
                        .iter()
                        .map(|(_, _, question)| question.clone())
                        .collect(),
                };
                let decision = match wait_for_user_question_answer(
                    state,
                    tx,
                    turn,
                    call,
                    &request,
                    cancel_token,
                )
                .await
                {
                    Ok(decision) => decision,
                    Err(_) => muse_runtime::interactions::UserQuestionDecision {
                        answered: false,
                        answers: None,
                        annotations: None,
                        reason: Some("memory_confirmation_interrupted".to_string()),
                    },
                };
                if decision.answered
                    && let Some(answers) = decision
                        .answers
                        .as_ref()
                        .and_then(|value| value.as_object())
                {
                    for (index, question, _) in &confirmation_questions {
                        let accepted = answers.get(question).is_some_and(|answer| match answer {
                            serde_json::Value::String(value) => value == "确认保存",
                            serde_json::Value::Array(values) => values
                                .iter()
                                .any(|value| value.as_str() == Some("确认保存")),
                            _ => false,
                        });
                        if accepted {
                            confirmed.insert(*index);
                        }
                    }
                }
            }
            let now = chrono::Utc::now().to_rfc3339();
            for (index, proposal) in proposals {
                if proposal.confirmation == MemoryConfirmationMode::AskUser
                    && !confirmed.contains(&index)
                {
                    receipts.push(memory_item_receipt(
                        index,
                        MemoryMutationItemState::Skipped,
                        Some(&proposal),
                        None,
                        None,
                        Some("memory_confirmation_skipped"),
                        Some("confirmation"),
                    ));
                    continue;
                }
                let operation_call_id = format!("{}-{index}", call.call_id);
                let binding = match proposal.source_quote.as_deref() {
                    Some(source_quote)
                        if proposal.confirmation == MemoryConfirmationMode::AskUser =>
                    {
                        memory_turn.bind_confirmed_user_mutation(
                            scope.clone(),
                            &turn.conversation_id,
                            &turn.turn_id,
                            &operation_call_id,
                            source_quote,
                            &now,
                            conversation,
                        )
                    }
                    Some(source_quote) => memory_turn.bind_direct_user_quote_mutation(
                        scope.clone(),
                        &turn.conversation_id,
                        &turn.turn_id,
                        &operation_call_id,
                        source_quote,
                        &now,
                        conversation,
                    ),
                    None => memory_turn.bind_direct_user_mutation(
                        scope.clone(),
                        &turn.conversation_id,
                        &turn.turn_id,
                        &operation_call_id,
                        &proposal.content,
                        &now,
                        conversation,
                    ),
                };
                let binding = match binding {
                    Ok(binding) => binding,
                    Err(error) => {
                        receipts.push(memory_item_receipt(
                            index,
                            MemoryMutationItemState::Rejected,
                            Some(&proposal),
                            None,
                            None,
                            Some(memory_error_code_value(error.code())),
                            Some("source_quote"),
                        ));
                        continue;
                    }
                };
                let params = match proposal.to_params() {
                    Ok(params) => params,
                    Err(error) => {
                        receipts.push(memory_item_receipt(
                            index,
                            MemoryMutationItemState::Rejected,
                            Some(&proposal),
                            None,
                            None,
                            Some(memory_error_code_value(error.code())),
                            Some("mutations"),
                        ));
                        continue;
                    }
                };
                let assigned_memory_id = params
                    .memory_id()
                    .cloned()
                    .unwrap_or_else(|| MemoryId(next_runtime_id("memory")));
                let assigned_revision_id = MemoryRevisionId(next_runtime_id("memory-rev"));
                let staged = match MemoryStagedMutation::stage_proposal(
                    &proposal,
                    binding,
                    assigned_memory_id.clone(),
                    assigned_revision_id.clone(),
                    services.sensitivity.as_ref(),
                ) {
                    Ok(staged) => staged,
                    Err(error) => {
                        receipts.push(memory_item_receipt(
                            index,
                            MemoryMutationItemState::Rejected,
                            Some(&proposal),
                            None,
                            None,
                            Some(memory_error_code_value(error.code())),
                            Some("content"),
                        ));
                        continue;
                    }
                };
                memory_turn.stage(staged);
                receipts.push(memory_item_receipt(
                    index,
                    MemoryMutationItemState::Staged,
                    Some(&proposal),
                    Some(assigned_memory_id),
                    Some(assigned_revision_id),
                    None,
                    None,
                ));
            }
            receipts.sort_by_key(|receipt| receipt.index);
            let staged_count = receipts
                .iter()
                .filter(|receipt| receipt.state == MemoryMutationItemState::Staged)
                .count();
            let skipped_count = receipts
                .iter()
                .filter(|receipt| receipt.state == MemoryMutationItemState::Skipped)
                .count();
            let rejected_count = receipts
                .iter()
                .filter(|receipt| receipt.state == MemoryMutationItemState::Rejected)
                .count();
            let confirmation_required_count = confirmation_questions.len();
            let state_value = if staged_count == 0 {
                MemoryMutationBatchState::Rejected
            } else if skipped_count > 0 || rejected_count > 0 {
                MemoryMutationBatchState::Partial
            } else {
                MemoryMutationBatchState::Staged
            };
            let receipt = MemoryMutationBatchReceipt {
                state: state_value,
                staged_count,
                confirmation_required_count,
                rejected_count,
                skipped_count,
                items: receipts,
            };
            ToolResult {
                status: if staged_count > 0 {
                    ToolResultStatus::Success
                } else {
                    ToolResultStatus::Failed
                },
                content: if staged_count > 0 {
                    format!(
                        "已暂存 {staged_count} 条记忆候选，等待本轮可靠提交；跳过 {skipped_count} 条，拒绝 {rejected_count} 条。不得声称已经保存。"
                    )
                } else {
                    format!(
                        "本轮没有可暂存的记忆；跳过 {skipped_count} 条，拒绝 {rejected_count} 条。不得声称已经保存。"
                    )
                },
                structured: serde_json::to_value(&receipt).ok(),
            }
        })
    }
}

fn parse_memory_mutation_proposals(
    arguments: &serde_json::Value,
) -> Result<ParsedMemoryMutationBatch, ToolResult> {
    if let Some(values) = arguments.get("mutations") {
        let values = values
            .as_array()
            .ok_or_else(|| memory_mutate_failure(MemoryErrorCode::InvalidRequest))?;
        if values.is_empty() || values.len() > MAX_MEMORY_MUTATIONS {
            return Err(memory_mutate_failure(MemoryErrorCode::InvalidRequest));
        }
        let mut proposals = Vec::new();
        let mut receipts = Vec::new();
        for (index, value) in values.iter().enumerate() {
            match serde_json::from_value::<MemoryMutationProposal>(value.clone()) {
                Ok(proposal) => match proposal.validate() {
                    Ok(()) => proposals.push((index, proposal)),
                    Err(error) => receipts.push(memory_item_receipt(
                        index,
                        MemoryMutationItemState::Rejected,
                        Some(&proposal),
                        None,
                        None,
                        Some(memory_error_code_value(error.code())),
                        Some(memory_proposal_field_path(&proposal)),
                    )),
                },
                Err(_) => receipts.push(memory_item_receipt(
                    index,
                    MemoryMutationItemState::Rejected,
                    None,
                    None,
                    None,
                    Some("memory_invalid_request"),
                    Some("mutations"),
                )),
            }
        }
        return Ok((proposals, receipts));
    }
    let request = serde_json::from_value::<MemoryMutateRequest>(arguments.clone())
        .map_err(|_| memory_mutate_failure(MemoryErrorCode::InvalidRequest))?;
    Ok((
        request.mutations.into_iter().enumerate().collect(),
        Vec::new(),
    ))
}

fn memory_proposal_field_path(proposal: &MemoryMutationProposal) -> &'static str {
    if proposal.content.trim().is_empty() {
        return "content";
    }
    if proposal.change_reason.trim().is_empty() {
        return "change_reason";
    }
    if proposal
        .source_quote
        .as_deref()
        .is_some_and(|source_quote| source_quote.trim().is_empty())
    {
        return "source_quote";
    }
    if proposal.keywords.is_empty()
        || proposal
            .keywords
            .iter()
            .any(|keyword| keyword.trim().is_empty())
    {
        return "keywords";
    }
    if !proposal.facet.is_compatible_with(proposal.category) {
        return "facet";
    }
    if proposal
        .event_time
        .as_deref()
        .is_some_and(|event_time| chrono::DateTime::parse_from_rfc3339(event_time).is_err())
    {
        return "event_time";
    }
    match proposal.operation {
        muse_core::domain::memory::MemoryChangeType::Create
            if proposal.memory_id.is_some() || proposal.expected_revision_id.is_some() =>
        {
            "operation"
        }
        muse_core::domain::memory::MemoryChangeType::Update
        | muse_core::domain::memory::MemoryChangeType::Correct
            if proposal.memory_id.is_none() =>
        {
            "memory_id"
        }
        muse_core::domain::memory::MemoryChangeType::Update
        | muse_core::domain::memory::MemoryChangeType::Correct
            if proposal.expected_revision_id.is_none() =>
        {
            "expected_revision_id"
        }
        _ => "mutations",
    }
}

#[allow(clippy::too_many_arguments)]
fn memory_item_receipt(
    index: usize,
    state: MemoryMutationItemState,
    proposal: Option<&MemoryMutationProposal>,
    memory_id: Option<MemoryId>,
    revision_id: Option<MemoryRevisionId>,
    reason_code: Option<&str>,
    field_path: Option<&str>,
) -> MemoryMutationItemReceipt {
    MemoryMutationItemReceipt {
        index,
        state,
        operation: proposal.map(|proposal| proposal.operation),
        memory_id,
        revision_id,
        reason_code: reason_code.map(str::to_string),
        field_path: field_path.map(|field| format!("mutations[{index}].{field}")),
    }
}

fn memory_error_code_value(code: MemoryErrorCode) -> &'static str {
    match code {
        MemoryErrorCode::InvalidRequest => "memory_invalid_request",
        MemoryErrorCode::InvalidStateTransition => "memory_invalid_state_transition",
        MemoryErrorCode::MemoryNotFound => "memory_not_found",
        MemoryErrorCode::RevisionConflict => "memory_revision_conflict",
        MemoryErrorCode::PersonaScopeMismatch => "memory_persona_scope_mismatch",
        MemoryErrorCode::SourceIneligible => "memory_source_ineligible",
        MemoryErrorCode::SensitiveContentRejected => "memory_sensitive_content_rejected",
        MemoryErrorCode::SensitivityUnavailable => "memory_sensitivity_unavailable",
        MemoryErrorCode::InvalidCursor => "memory_cursor_invalid",
        MemoryErrorCode::CursorExpired => "memory_cursor_expired",
        MemoryErrorCode::QueryRejected => "memory_query_rejected",
        MemoryErrorCode::QueryBudgetExceeded => "memory_query_budget_exceeded",
        MemoryErrorCode::DeleteConfirmationRequired => "memory_delete_confirmation_required",
        MemoryErrorCode::DeletionAuthorityUnavailable => "memory_deletion_authority_unavailable",
        MemoryErrorCode::DeletionIncomplete => "memory_deletion_incomplete",
        MemoryErrorCode::RepositoryUnavailable => "memory_repository_unavailable",
    }
}

impl RuntimeToolHandler for MemoryDeleteHandler {
    fn name(&self) -> &'static str {
        MEMORY_DELETE_TOOL_NAME
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        let params: MemoryDeleteParams = serde_json::from_value(call.arguments.clone())
            .map_err(|_| memory_tool_failure(MemoryErrorCode::InvalidRequest))?;
        params.validate().map_err(memory_tool_failed)
    }

    fn check_permissions(
        &self,
        _state: &Arc<AppState>,
        turn: &TurnContext,
        call: &ToolCall,
    ) -> Result<(), ToolResult> {
        check_memory_permissions(turn, call)
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match serde_json::from_value::<MemoryDeleteParams>(call.arguments.clone()) {
            Ok(MemoryDeleteParams::Memory { memory_id }) => {
                format!("永久删除记忆 {} 及其全部历史", memory_id.0)
            }
            Ok(MemoryDeleteParams::PersonaAll) => "永久删除当前角色的全部记忆及历史".to_string(),
            Err(_) => "永久删除长期记忆".to_string(),
        }
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            let RuntimeToolInvocation {
                state,
                turn,
                call,
                approval_obtained,
                approval_evidence,
                ..
            } = invocation;
            let Some(approval_evidence) = approval_evidence
                .filter(|evidence| approval_obtained && evidence.call_id == call.call_id)
            else {
                return memory_delete_confirmation_required();
            };
            let Some(services) = state.memory.as_ref() else {
                return memory_tool_failed(MemoryError::new(
                    MemoryErrorCode::RepositoryUnavailable,
                ));
            };
            let Ok(params) = serde_json::from_value::<MemoryDeleteParams>(call.arguments.clone())
            else {
                return memory_tool_failure(MemoryErrorCode::InvalidRequest);
            };
            let scope = match memory_scope_for_turn(turn, MemoryErrorCode::PersonaScopeMismatch) {
                Ok(scope) => scope,
                Err(result) => return result,
            };
            let confirmation = match MemoryDeleteConfirmation::new(
                approval_evidence.approval_id.clone(),
                &scope,
                &params,
                approval_evidence.approved_at.clone(),
                approval_evidence.expires_at.clone(),
                MemoryDeleteConfirmationSource::ConversationTurn {
                    conversation_id: turn.conversation_id.clone(),
                    turn_id: turn.turn_id.clone(),
                    approval_id: approval_evidence.approval_id.clone(),
                    // 供应商返回的调用 ID 属于不可信输入；删除确认必须复用 Session/SSE 的安全规范化口径。
                    call_id: memory_session_call_id(&call.call_id),
                },
            ) {
                Ok(confirmation) => confirmation,
                Err(error) => return memory_tool_failed(error),
            };
            let request = match ConfirmedMemoryDeleteRequest::bind(params, scope, confirmation) {
                Ok(request) => request,
                Err(error) => return memory_tool_failed(error),
            };
            let receipt = match services
                .repository
                .delete_confirmed(&request, services.deletion_authority.as_ref())
            {
                Ok(receipt) => receipt,
                Err(error) => return memory_tool_failed(error),
            };
            ToolResult {
                status: ToolResultStatus::Success,
                content: "记忆已永久删除。".to_string(),
                structured: serde_json::to_value(&receipt).ok(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_without_dedicated_confirmation_uses_stable_failure() {
        let result = memory_delete_confirmation_required();
        assert_eq!(result.status, ToolResultStatus::Failed);
        assert_eq!(
            result.structured,
            Some(serde_json::json!({
                "error_code": MemoryErrorCode::DeleteConfirmationRequired,
            }))
        );
    }

    #[test]
    fn every_memory_failure_uses_the_session_consumer_contract() {
        let codes = [
            MemoryErrorCode::InvalidRequest,
            MemoryErrorCode::InvalidStateTransition,
            MemoryErrorCode::MemoryNotFound,
            MemoryErrorCode::RevisionConflict,
            MemoryErrorCode::PersonaScopeMismatch,
            MemoryErrorCode::SourceIneligible,
            MemoryErrorCode::SensitiveContentRejected,
            MemoryErrorCode::SensitivityUnavailable,
            MemoryErrorCode::InvalidCursor,
            MemoryErrorCode::CursorExpired,
            MemoryErrorCode::QueryRejected,
            MemoryErrorCode::QueryBudgetExceeded,
            MemoryErrorCode::DeleteConfirmationRequired,
            MemoryErrorCode::DeletionAuthorityUnavailable,
            MemoryErrorCode::DeletionIncomplete,
            MemoryErrorCode::RepositoryUnavailable,
        ];
        for code in codes {
            let result = memory_tool_failure(code);
            assert_eq!(result.status, ToolResultStatus::Failed);
            assert_eq!(result.content, MemoryError::new(code).to_string());
            let structured = result.structured.expect("记忆失败必须提供稳定错误码");
            assert_eq!(structured, serde_json::json!({ "error_code": code }));
            assert_eq!(structured.as_object().map(serde_json::Map::len), Some(1));
            assert!(structured.get("reason").is_none());

            let provider_event = runtime_tool_result_event(
                "call-memory-failure",
                MEMORY_MUTATE_TOOL_NAME,
                false,
                &result.content,
                Some(&structured),
            );
            assert_eq!(provider_event["structured"], structured);
            assert_eq!(
                provider_event["structured"]["error_code"],
                serde_json::json!(code)
            );
        }
    }

    #[test]
    fn batch_parser_keeps_valid_items_when_another_item_is_invalid() {
        let (proposals, receipts) = parse_memory_mutation_proposals(&serde_json::json!({
            "mutations": [
                {
                    "operation": "create",
                    "source_quote": "我喜欢吃酸菜鱼",
                    "category": "user_preference",
                    "facet": "preference_food",
                    "keywords": ["酸菜鱼", "食物偏好"],
                    "content": "用户喜欢吃酸菜鱼。",
                    "importance": "normal",
                    "change_reason": "用户直接表达饮食偏好",
                    "confirmation": "not_required"
                },
                {
                    "operation": "create",
                    "source_quote": "我喜欢喝咖啡",
                    "category": "user_preference",
                    "facet": "preference_drink",
                    "keywords": [],
                    "content": "用户喜欢喝咖啡。",
                    "importance": "normal",
                    "change_reason": "用户直接表达饮品偏好",
                    "confirmation": "not_required"
                }
            ]
        }))
        .expect("批量 envelope 应可解析");

        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].0, 0);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].index, 1);
        assert_eq!(receipts[0].state, MemoryMutationItemState::Rejected);
        assert_eq!(
            receipts[0].reason_code.as_deref(),
            Some("memory_invalid_request")
        );
        assert_eq!(
            receipts[0].field_path.as_deref(),
            Some("mutations[1].keywords")
        );
    }

    #[test]
    fn pre_handler_failure_only_matches_exact_memory_tool_names() {
        for (tool_name, code) in [
            (MEMORY_QUERY_TOOL_NAME, MemoryErrorCode::QueryRejected),
            (MEMORY_MUTATE_TOOL_NAME, MemoryErrorCode::InvalidRequest),
            (MEMORY_DELETE_TOOL_NAME, MemoryErrorCode::InvalidRequest),
        ] {
            let result = memory_pre_handler_failure(tool_name).expect("精确记忆工具名必须命中");
            assert_eq!(
                result.structured,
                Some(serde_json::json!({ "error_code": code }))
            );
        }
        for tool_name in ["memory_query_extra", "Memory_query", "file_read"] {
            assert!(memory_pre_handler_failure(tool_name).is_none());
        }
    }

    #[test]
    fn delete_approval_summary_marks_irreversible_scope() {
        let handler = MemoryDeleteHandler;
        let single = ToolCall {
            call_id: "call-delete-one".to_string(),
            name: MEMORY_DELETE_TOOL_NAME.to_string(),
            arguments: serde_json::json!({
                "scope": "memory",
                "memory_id": "memory-one"
            }),
            source: muse_core::domain::tool::ToolCallSource::TextJsonFallback,
        };
        assert_eq!(
            handler.approval_summary(&single),
            "永久删除记忆 memory-one 及其全部历史"
        );

        let all = ToolCall {
            call_id: "call-delete-all".to_string(),
            name: MEMORY_DELETE_TOOL_NAME.to_string(),
            arguments: serde_json::json!({ "scope": "persona_all" }),
            source: muse_core::domain::tool::ToolCallSource::TextJsonFallback,
        };
        assert_eq!(
            handler.approval_summary(&all),
            "永久删除当前角色的全部记忆及历史"
        );
    }
}
