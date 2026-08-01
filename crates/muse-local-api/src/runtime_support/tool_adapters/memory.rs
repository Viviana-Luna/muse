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
    MemoryRuntimeBinding, MemorySourceKind, MemoryStagedMutation,
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
    tool_failed(error.to_string(), error.stable_code())
}

fn memory_tool_failed_code(code: MemoryErrorCode, message: impl Into<String>) -> ToolResult {
    tool_failed(message, code.as_str())
}

/// memory_delete 未取得专用真人确认时的唯一模型可见失败形态。
pub(in crate::runtime_support) fn memory_delete_confirmation_required(outcome: &str) -> ToolResult {
    ToolResult {
        status: ToolResultStatus::Failed,
        content: "删除记忆需要专用用户确认，本次未删除任何内容。".to_string(),
        structured: Some(serde_json::json!({
            "reason": MemoryErrorCode::DeleteConfirmationRequired.as_str(),
            "confirmation_outcome": outcome,
        })),
    }
}

/// 从冻结 TurnContext 解析 persona scope；无活动角色时记忆工具一律拒绝。
fn memory_scope_for_turn(
    turn: &TurnContext,
    code: MemoryErrorCode,
) -> Result<MemoryPersonaScope, ToolResult> {
    let Some(persona_id) = turn.persona_id.as_deref() else {
        return Err(memory_tool_failed_code(
            code,
            "当前对话没有活动角色，记忆工具不可用。",
        ));
    };
    MemoryPersonaScope::new(persona_id).map_err(memory_tool_failed)
}

impl RuntimeToolHandler for MemoryQueryHandler {
    fn name(&self) -> &'static str {
        MEMORY_QUERY_TOOL_NAME
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        let params: MemoryQueryParams =
            serde_json::from_value(call.arguments.clone()).map_err(|_| {
                memory_tool_failed_code(
                    MemoryErrorCode::InvalidRequest,
                    "memory_query 参数不符合冻结 schema。",
                )
            })?;
        params.validate().map_err(memory_tool_failed)
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
                return memory_tool_failed_code(
                    MemoryErrorCode::QueryRejected,
                    "记忆检索当前未接线，本次无法查询长期记忆。",
                );
            };
            let Ok(params) = serde_json::from_value::<MemoryQueryParams>(call.arguments.clone())
            else {
                return memory_tool_failed_code(
                    MemoryErrorCode::InvalidRequest,
                    "memory_query 参数不符合冻结 schema。",
                );
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
        let params: MemoryMutateParams =
            serde_json::from_value(call.arguments.clone()).map_err(|_| {
                memory_tool_failed_code(
                    MemoryErrorCode::InvalidRequest,
                    "memory_mutate 参数不符合冻结 schema。",
                )
            })?;
        params.validate().map_err(memory_tool_failed)
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
                ..
            } = invocation;
            let Some(services) = state.memory.as_ref() else {
                return memory_tool_failed(MemoryError::new(
                    MemoryErrorCode::RepositoryUnavailable,
                ));
            };
            let Ok(params) = serde_json::from_value::<MemoryMutateParams>(call.arguments.clone())
            else {
                return memory_tool_failed_code(
                    MemoryErrorCode::InvalidRequest,
                    "memory_mutate 参数不符合冻结 schema。",
                );
            };
            // 来源资格在结构上冻结：处理器只能以当前 Turn 直接用户消息构造绑定，
            // assistant-only 断言、system/reasoning、Tool/MCP 结果、网页文件都无法经
            // 该入口形成记忆；失败或取消的 Turn 不会走到 committed，暂存随作用域丢弃。
            let scope = match memory_scope_for_turn(turn, MemoryErrorCode::SourceIneligible) {
                Ok(scope) => scope,
                Err(result) => return result,
            };
            let now = chrono::Utc::now().to_rfc3339();
            let binding = match MemoryRuntimeBinding::new(
                scope,
                turn.conversation_id.clone(),
                turn.turn_id.clone(),
                next_runtime_id("memory-op"),
                MemorySourceKind::DirectUserMessage,
                now.clone(),
                now.clone(),
                now,
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
        let params: MemoryDeleteParams =
            serde_json::from_value(call.arguments.clone()).map_err(|_| {
                memory_tool_failed_code(
                    MemoryErrorCode::InvalidRequest,
                    "memory_delete 参数不符合冻结 schema。",
                )
            })?;
        params.validate().map_err(memory_tool_failed)
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
                ..
            } = invocation;
            if !approval_obtained {
                return memory_delete_confirmation_required("missing_dedicated_confirmation");
            }
            let Some(services) = state.memory.as_ref() else {
                return memory_tool_failed(MemoryError::new(
                    MemoryErrorCode::RepositoryUnavailable,
                ));
            };
            let Ok(params) = serde_json::from_value::<MemoryDeleteParams>(call.arguments.clone())
            else {
                return memory_tool_failed_code(
                    MemoryErrorCode::InvalidRequest,
                    "memory_delete 参数不符合冻结 schema。",
                );
            };
            let scope = match memory_scope_for_turn(turn, MemoryErrorCode::PersonaScopeMismatch) {
                Ok(scope) => scope,
                Err(result) => return result,
            };
            let now = chrono::Utc::now().to_rfc3339();
            let confirmation = match MemoryDeleteConfirmation::new(
                next_runtime_id("memory-confirm"),
                now,
                MemoryDeleteConfirmationSource::ConversationTurn {
                    conversation_id: turn.conversation_id.clone(),
                    turn_id: turn.turn_id.clone(),
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
        let result = memory_delete_confirmation_required("rejected");
        assert_eq!(result.status, ToolResultStatus::Failed);
        assert_eq!(
            result
                .structured
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str),
            Some(MemoryErrorCode::DeleteConfirmationRequired.as_str())
        );
        assert_eq!(
            result
                .structured
                .as_ref()
                .and_then(|value| value.get("confirmation_outcome"))
                .and_then(serde_json::Value::as_str),
            Some("rejected")
        );
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
