//! intrinsic 记忆工具的运行时处理器。
//!
//! 三个处理器只承接 F1 冻结的领域契约：查询绑定冻结 TurnContext 的 persona scope
//! （模型永远不能传 persona_id）；变更只进 Turn 局部暂存并在 committed 后原子提交；
//! 删除必须走专用用户确认，普通审批、角色提示词或模型自述都不能替代。

use super::*;

use muse_core::domain::memory::{
    ConfirmedMemoryDeleteRequest, MEMORY_DELETE_TOOL_NAME, MEMORY_MUTATE_TOOL_NAME,
    MEMORY_QUERY_TOOL_NAME, MemoryDeleteConfirmation, MemoryDeleteConfirmationSource,
    MemoryDeleteParams, MemoryError, MemoryErrorCode, MemoryId, MemoryMutateParams,
    MemoryPersonaScope, MemoryQueryParams, MemoryRetrievalRequest, MemoryRevisionId,
    MemoryStagedMutation,
};

pub(in crate::runtime_support) struct MemoryQueryHandler;
pub(in crate::runtime_support) struct MemoryMutateHandler;
pub(in crate::runtime_support) struct MemoryDeleteHandler;

pub(in crate::runtime_support) static MEMORY_QUERY_HANDLER: MemoryQueryHandler = MemoryQueryHandler;
pub(in crate::runtime_support) static MEMORY_MUTATE_HANDLER: MemoryMutateHandler =
    MemoryMutateHandler;
pub(in crate::runtime_support) static MEMORY_DELETE_HANDLER: MemoryDeleteHandler =
    MemoryDeleteHandler;

fn memory_tool_failed(error: MemoryError) -> ToolResult {
    memory_tool_failure(error.code())
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
            let request = match MemoryRetrievalRequest::bind(params, scope) {
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
        let params: MemoryMutateParams = serde_json::from_value(call.arguments.clone())
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

    fn is_mutating(&self, _call: &ToolCall) -> bool {
        // 该调用只写 Turn 局部暂存，不产生外部副作用；durable 提交由 committed 之后的
        // 批量路径完成。标 mutating 会让取消中的 Turn 被误记为已产生外部效果。
        false
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            let RuntimeToolInvocation {
                state,
                turn,
                call,
                memory_turn,
                conversation,
                ..
            } = invocation;
            let Some(services) = state.memory.as_ref() else {
                return memory_tool_failed(MemoryError::new(
                    MemoryErrorCode::RepositoryUnavailable,
                ));
            };
            let Ok(params) = serde_json::from_value::<MemoryMutateParams>(call.arguments.clone())
            else {
                return memory_tool_failure(MemoryErrorCode::InvalidRequest);
            };
            // 来源资格由 Turn 开始时冻结的 API 用户输入签发，候选事实还必须能在
            // 当前最新用户消息的统一规范化正文中确定性验证；模型不能自报来源。
            let scope = match memory_scope_for_turn(turn, MemoryErrorCode::SourceIneligible) {
                Ok(scope) => scope,
                Err(result) => return result,
            };
            let now = chrono::Utc::now().to_rfc3339();
            let binding = match memory_turn.bind_direct_user_mutation(
                scope,
                &turn.conversation_id,
                &turn.turn_id,
                &call.call_id,
                params.content(),
                &now,
                conversation,
            ) {
                Ok(binding) => binding,
                Err(error) => return memory_tool_failed(error),
            };
            let assigned_memory_id = params
                .memory_id()
                .cloned()
                .unwrap_or_else(|| MemoryId(next_runtime_id("memory")));
            let assigned_revision_id = MemoryRevisionId(next_runtime_id("memory-rev"));
            let staged = match MemoryStagedMutation::stage(
                params,
                binding,
                assigned_memory_id,
                assigned_revision_id,
                services.sensitivity.as_ref(),
            ) {
                Ok(staged) => staged,
                Err(error) => return memory_tool_failed(error),
            };
            let receipt = staged.staged_receipt();
            memory_turn.stage(staged);
            ToolResult {
                status: ToolResultStatus::Success,
                // staged 只表示已接受、等待本轮提交，绝不能向模型承诺已 durable。
                content: "已暂存，等待本轮对话可靠提交后生效。".to_string(),
                structured: serde_json::to_value(&receipt).ok(),
            }
        })
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
                    call_id: call.call_id.clone(),
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
