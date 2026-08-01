//! 运行时工具注册、权限边界与分派实现。

use super::*;
use muse_core::domain::memory::MEMORY_DELETE_TOOL_NAME;

pub(in crate::runtime_support) struct RuntimeToolExecutionContext<'a> {
    pub(in crate::runtime_support) snapshot: &'a TurnSnapshot,
    pub(in crate::runtime_support) frozen_mcp_catalog: &'a mcp::McpToolCatalog,
    pub(in crate::runtime_support) provider:
        &'a Arc<dyn muse_core::model::provider::ChatModelProvider>,
    pub(in crate::runtime_support) request_capability_epoch: u64,
    pub(in crate::runtime_support) turn: &'a TurnContext,
    pub(in crate::runtime_support) cancel_token: &'a RuntimeTurnCancel,
    pub(in crate::runtime_support) conversation: &'a Conversation,
    pub(in crate::runtime_support) memory_turn: &'a MemoryTurnState,
}
pub(in crate::runtime_support) async fn execute_runtime_tool(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    context: RuntimeToolExecutionContext<'_>,
    call: ToolCall,
) -> Result<ToolResult, String> {
    let RuntimeToolExecutionContext {
        snapshot,
        frozen_mcp_catalog,
        provider,
        request_capability_epoch,
        turn,
        cancel_token,
        conversation,
        memory_turn,
    } = context;
    if cancel_token.is_cancelled() {
        return Err(TURN_CANCELLED_MESSAGE.to_string());
    }
    if let Err(error) = snapshot.authorize_tool_call(&call.name, request_capability_epoch) {
        let result = memory_pre_handler_failure(&call.name)
            .unwrap_or_else(|| tool_failed(error, "stale_or_hidden_capability"));
        emit_and_record_tool_call(
            state,
            tx,
            turn,
            &call,
            "unknown",
            snapshot.tool_definition(&call.name),
            RuntimeToolExecutionPolicy::unknown(),
        )
        .await?;
        return emit_and_record_tool_result(state, tx, turn, &call, &result).await;
    }
    let Some(def) = snapshot.tool_definition(&call.name).cloned() else {
        let result = memory_pre_handler_failure(&call.name).unwrap_or_else(|| ToolResult {
            status: ToolResultStatus::Failed,
            content: format!("工具 `{}` 不存在，无法执行。", call.name),
            structured: Some(serde_json::json!({ "reason": "not_found" })),
        });
        emit_and_record_tool_call(
            state,
            tx,
            turn,
            &call,
            "unknown",
            None,
            RuntimeToolExecutionPolicy::unknown(),
        )
        .await
        .map_err(|_| "工具审批等待被中断。".to_string())?;
        let recorded_result = emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
        return Ok(recorded_result);
    };
    if !def.available {
        let result = memory_pre_handler_failure(&call.name).unwrap_or_else(|| ToolResult {
            status: ToolResultStatus::Failed,
            content: format!(
                "工具 `{}` 当前不可用：{}",
                call.name,
                def.disabled_reason
                    .as_deref()
                    .unwrap_or("未提供不可用原因。")
            ),
            structured: Some(serde_json::json!({
                "reason": "disabled",
                "tool": call.name,
                "execution_owner": def.execution_owner,
                "disabled_reason": def.disabled_reason,
            })),
        });
        emit_and_record_tool_call(
            state,
            tx,
            turn,
            &call,
            def.risk.as_str(),
            Some(&def),
            RuntimeToolExecutionPolicy::unknown(),
        )
        .await?;
        let recorded_result = emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
        return Ok(recorded_result);
    }
    let active_preset =
        ToolPreset::from_protocol(&turn.tool_preset).unwrap_or(ToolPreset::FocusBuild);
    if !ToolRegistry::is_tool_definition_allowed_for_preset(&def, active_preset) {
        let result = memory_pre_handler_failure(&call.name).unwrap_or_else(|| ToolResult {
            status: ToolResultStatus::Failed,
            content: format!(
                "当前运行模式的工具预设 `{}` 未开放工具 `{}`。",
                active_preset.as_str(),
                call.name
            ),
            structured: Some(serde_json::json!({
                "reason": "preset_denied",
                "tool_preset": active_preset.as_str(),
                "turn_tool_preset": turn.tool_preset,
            })),
        });
        emit_and_record_tool_call(
            state,
            tx,
            turn,
            &call,
            def.risk.as_str(),
            Some(&def),
            RuntimeToolExecutionPolicy::unknown(),
        )
        .await?;
        let recorded_result = emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
        return Ok(recorded_result);
    }

    let current_policy = state
        .runtime_service
        .execution_policy()
        .map_err(|err| err.to_string())?;
    let execution_boundary = snapshot.execution_policy().restricted_by(&current_policy);
    let runtime_handler = runtime_tool_handler(&call.name);
    let preflight_policy =
        RuntimeToolExecutionPolicy::from_handler(runtime_handler, &call, &def, false, false);

    if let Some(handler) = runtime_handler {
        if let Err(result) = handler.validate_input(&call) {
            emit_and_record_tool_call(
                state,
                tx,
                turn,
                &call,
                def.risk.as_str(),
                Some(&def),
                preflight_policy,
            )
            .await?;
            let recorded_result =
                emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
            return Ok(recorded_result);
        }

        if let Err(result) = handler.check_permissions(state, turn, &call) {
            emit_and_record_tool_call(
                state,
                tx,
                turn,
                &call,
                def.risk.as_str(),
                Some(&def),
                preflight_policy,
            )
            .await?;
            let recorded_result =
                emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
            return Ok(recorded_result);
        }
    } else if !runtime_tool_allowed(turn, &call.name) {
        let result = ToolResult {
            status: ToolResultStatus::Failed,
            content: format!("当前角色未被授权调用工具 `{}`。", call.name),
            structured: Some(serde_json::json!({ "reason": "unauthorized" })),
        };
        emit_and_record_tool_call(
            state,
            tx,
            turn,
            &call,
            def.risk.as_str(),
            Some(&def),
            preflight_policy,
        )
        .await?;
        let recorded_result = emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
        return Ok(recorded_result);
    }

    let requires_workspace_boundary_approval = runtime_handler
        .map(|handler| handler.requires_workspace_boundary_approval(&execution_boundary, &call))
        .unwrap_or(false);
    // memory_delete 的专用用户确认不能被全局 Never 或 AUTO 审查策略绕过。
    let requires_approval = call.name == MEMORY_DELETE_TOOL_NAME
        || should_require_tool_approval(
            &execution_boundary,
            &call,
            def.risk.as_str(),
            def.requires_approval,
        )
        || requires_workspace_boundary_approval;
    let execution_policy = RuntimeToolExecutionPolicy::from_handler(
        runtime_handler,
        &call,
        &def,
        requires_approval,
        requires_workspace_boundary_approval,
    );

    emit_and_record_tool_call(
        state,
        tx,
        turn,
        &call,
        def.risk.as_str(),
        Some(&def),
        execution_policy,
    )
    .await?;

    let mut approval_obtained = false;
    let mut approval_evidence = None;
    let mut allow_approved_external_path = false;
    if requires_approval {
        let mut summary = runtime_handler
            .map(|handler| handler.approval_summary(&call))
            .unwrap_or_else(|| {
                if mcp::is_external_mcp_tool_name(&call.name) {
                    format!("调用外部 MCP 工具：{}", call.name)
                } else {
                    format!("执行工具：{}", call.name)
                }
            });
        if requires_workspace_boundary_approval {
            summary.push_str("\n目标路径不在当前允许工作区内；允许后仅本次工具调用可访问该路径。");
        }
        let approvals_reviewer =
            if call.name == MEMORY_DELETE_TOOL_NAME || mcp::is_external_mcp_tool_name(&call.name) {
                ApprovalsReviewer::User
            } else {
                execution_boundary.approvals_reviewer
            };
        let approval_risk = if call.name == MEMORY_DELETE_TOOL_NAME {
            MEMORY_DELETE_TOOL_NAME
        } else {
            def.risk.as_str()
        };
        let (approved, approval_reason, resolved_evidence) = wait_for_tool_approval(
            state,
            ToolApprovalRequest {
                tx,
                turn,
                call: &call,
                risk: approval_risk,
                summary,
                cancel_token,
                provider,
                conversation,
                approvals_reviewer,
                policy_revision: execution_boundary.revision,
            },
        )
        .await
        .map_err(|_| "工具审批等待被中断。".to_string())?;
        if !approved {
            let result = if call.name == MEMORY_DELETE_TOOL_NAME {
                memory_delete_confirmation_required()
            } else {
                let content = match approval_reason.as_str() {
                    "timeout" => format!("工具 `{}` 等待审批超时，已取消执行。", call.name),
                    "client_disconnected" => {
                        format!("工具 `{}` 因前端连接断开，已取消执行。", call.name)
                    }
                    reason if reason.contains("取消") => {
                        format!("工具 `{}` 已由用户取消，未执行。", call.name)
                    }
                    _ => format!("用户拒绝执行工具 `{}`。", call.name),
                };
                ToolResult {
                    status: ToolResultStatus::Failed,
                    content,
                    structured: Some(serde_json::json!({ "reason": approval_reason })),
                }
            };
            let recorded_result =
                emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
            return Ok(recorded_result);
        }
        if requires_workspace_boundary_approval {
            allow_approved_external_path = true;
        }
        approval_obtained = true;
        approval_evidence = resolved_evidence;
    }

    if cancel_token.is_cancelled() {
        return Err(TURN_CANCELLED_MESSAGE.to_string());
    }

    let result = if execution_policy.is_mutating {
        let _mutating_guard = state.mutating_tool_gate.lock().await;
        if cancel_token.is_cancelled() {
            return Err(TURN_CANCELLED_MESSAGE.to_string());
        }
        dispatch_runtime_tool_with_latest_policy(
            state,
            tx,
            snapshot,
            frozen_mcp_catalog,
            provider,
            turn,
            cancel_token,
            conversation,
            memory_turn,
            &call,
            &def,
            execution_policy,
            approval_obtained,
            approval_evidence.as_ref(),
            allow_approved_external_path,
        )
        .await
    } else {
        dispatch_runtime_tool_with_latest_policy(
            state,
            tx,
            snapshot,
            frozen_mcp_catalog,
            provider,
            turn,
            cancel_token,
            conversation,
            memory_turn,
            &call,
            &def,
            execution_policy,
            approval_obtained,
            approval_evidence.as_ref(),
            allow_approved_external_path,
        )
        .await
    }?;
    let cancelled_tool_result_recorded =
        cancel_token.is_cancelled() && tool_result_reason(&result) == Some("turn_cancelled");
    if cancel_token.is_cancelled() && !cancelled_tool_result_recorded {
        return Err(TURN_CANCELLED_MESSAGE.to_string());
    }
    if let Some(tx) = tx
        && call.name == "tts_speak"
        && result.is_success()
    {
        let speech_text = result
            .structured
            .as_ref()
            .and_then(|structured| structured.get("speech_text"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        emit_json_event(
            tx,
            runtime_speech_started_event(
                &call.call_id,
                speech_text,
                turn.active_voice_id.as_deref(),
            ),
        )
        .await
        .map_err(|_| "客户端连接已断开。".to_string())?;
    }
    let recorded_result = emit_and_record_tool_result(state, tx, turn, &call, &result).await?;
    if cancelled_tool_result_recorded {
        return Err(TURN_CANCELLED_MESSAGE.to_string());
    }
    Ok(recorded_result)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runtime_support) async fn dispatch_runtime_tool_with_latest_policy(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    snapshot: &TurnSnapshot,
    frozen_mcp_catalog: &mcp::McpToolCatalog,
    provider: &Arc<dyn muse_core::model::provider::ChatModelProvider>,
    turn: &TurnContext,
    cancel_token: &RuntimeTurnCancel,
    conversation: &Conversation,
    memory_turn: &MemoryTurnState,
    call: &ToolCall,
    definition: &ToolDef,
    execution_policy: RuntimeToolExecutionPolicy,
    approval_obtained: bool,
    approval_evidence: Option<&ToolApprovalEvidence>,
    allow_approved_external_path: bool,
) -> Result<ToolResult, String> {
    // 配置扩大仍受本轮冻结快照限制；配置撤销在真实 dispatch 前最后一次求交，
    // 防止审批等待或可变工具排队期间继续沿用已经撤销的权限。
    let current_policy = state
        .runtime_service
        .execution_policy()
        .map_err(|err| err.to_string())?;
    let execution_boundary = match execution_boundary_for_dispatch(
        snapshot,
        &current_policy,
        call,
        definition,
        approval_obtained,
        allow_approved_external_path,
    ) {
        Ok(boundary) => boundary,
        Err(result) => return Ok(result),
    };

    persist_external_effect_boundary_before_dispatch(
        state,
        turn,
        call,
        definition,
        execution_policy,
    )
    .await?;
    Ok(dispatch_runtime_tool(
        state,
        tx,
        RuntimeToolDispatchContext {
            execution_policy: &execution_boundary,
            frozen_mcp_catalog,
            provider,
            turn,
            cancel_token,
            conversation,
            memory_turn,
            approval_obtained,
            approval_evidence,
            allow_approved_external_path,
        },
        call,
    )
    .await)
}

pub(in crate::runtime_support) fn execution_boundary_for_dispatch(
    snapshot: &TurnSnapshot,
    current_policy: &FrozenExecutionPolicy,
    call: &ToolCall,
    definition: &ToolDef,
    approval_obtained: bool,
    allow_approved_external_path: bool,
) -> Result<FrozenExecutionPolicy, ToolResult> {
    let execution_boundary = snapshot.execution_policy().restricted_by(current_policy);
    let runtime_handler = runtime_tool_handler(&call.name);
    let boundary_approval_now = runtime_handler
        .map(|handler| handler.requires_workspace_boundary_approval(&execution_boundary, call))
        .unwrap_or(false);
    let approval_required_now = should_require_tool_approval(
        &execution_boundary,
        call,
        definition.risk.as_str(),
        definition.requires_approval,
    ) || boundary_approval_now;
    if approval_required_now && !approval_obtained {
        return Err(tool_failed(
            format!(
                "工具 `{}` 等待执行期间权限已收紧，当前调用需要重新审批。",
                call.name
            ),
            "execution_policy_revoked",
        ));
    }
    if boundary_approval_now && !allow_approved_external_path {
        return Err(tool_failed(
            format!(
                "工具 `{}` 的目标路径已不在当前允许根目录内，已拒绝执行。",
                call.name
            ),
            "execution_policy_revoked",
        ));
    }

    Ok(execution_boundary)
}

pub(in crate::runtime_support) async fn persist_external_effect_boundary_before_dispatch(
    state: &Arc<AppState>,
    turn: &TurnContext,
    call: &ToolCall,
    definition: &ToolDef,
    execution_policy: RuntimeToolExecutionPolicy,
) -> Result<(), String> {
    if execution_policy.is_mutating
        || matches!(
            definition.risk,
            ToolRisk::Network
                | ToolRisk::WriteFile
                | ToolRisk::ExecuteCommand
                | ToolRisk::ExternalSideEffect
        )
    {
        // 真实 dispatch 可能在返回 Failed 前已经部分改变外部状态。必需事件
        // 只记录调用身份和风险，不持久化参数或正文；写成功后才能越过边界。
        durable_effect_boundary_before_dispatch(
            || {
                append_required_turn_event(
                    state,
                    "turn_effect_started",
                    serde_json::json!({
                        "conversation_id": turn.conversation_id,
                        "turn_id": turn.turn_id,
                        "call_id": call.call_id,
                        "tool": call.name,
                        "risk": definition.risk.as_str(),
                    }),
                )
            },
            || mark_active_turn_external_effects(state, &turn.turn_id),
        )
        .await?;
    }
    Ok(())
}

pub(in crate::runtime_support) async fn durable_effect_boundary_before_dispatch<
    Persist,
    PersistFuture,
    Mark,
>(
    persist: Persist,
    mark: Mark,
) -> Result<(), String>
where
    Persist: FnOnce() -> PersistFuture,
    PersistFuture: Future<Output = Result<(), String>>,
    Mark: FnOnce() -> Result<(), String>,
{
    persist().await?;
    mark()?;
    Ok(())
}

pub(in crate::runtime_support) type RuntimeToolFuture<'a> =
    Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;

pub(in crate::runtime_support) struct RuntimeToolInvocation<'a> {
    pub(in crate::runtime_support) state: &'a Arc<AppState>,
    pub(in crate::runtime_support) tx: Option<&'a RuntimeSseSender>,
    pub(in crate::runtime_support) execution_policy: &'a FrozenExecutionPolicy,
    pub(in crate::runtime_support) frozen_mcp_catalog: &'a mcp::McpToolCatalog,
    pub(in crate::runtime_support) provider:
        &'a Arc<dyn muse_core::model::provider::ChatModelProvider>,
    pub(in crate::runtime_support) turn: &'a TurnContext,
    pub(in crate::runtime_support) call: &'a ToolCall,
    pub(in crate::runtime_support) cancel_token: &'a RuntimeTurnCancel,
    pub(in crate::runtime_support) conversation: &'a Conversation,
    pub(in crate::runtime_support) memory_turn: &'a MemoryTurnState,
    pub(in crate::runtime_support) approval_obtained: bool,
    pub(in crate::runtime_support) approval_evidence: Option<&'a ToolApprovalEvidence>,
    pub(in crate::runtime_support) allow_approved_external_path: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(in crate::runtime_support) struct AskUserQuestionOption {
    pub(in crate::runtime_support) label: String,
    pub(in crate::runtime_support) description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(in crate::runtime_support) struct AskUserQuestionItem {
    pub(in crate::runtime_support) question: String,
    pub(in crate::runtime_support) header: String,
    pub(in crate::runtime_support) options: Vec<AskUserQuestionOption>,
    #[serde(default, rename = "multiSelect", alias = "multi_select")]
    pub(in crate::runtime_support) multi_select: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(in crate::runtime_support) struct AskUserQuestionRequest {
    pub(in crate::runtime_support) questions: Vec<AskUserQuestionItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(in crate::runtime_support) struct TodoWriteItemInput {
    #[serde(default)]
    pub(in crate::runtime_support) id: Option<String>,
    pub(in crate::runtime_support) content: String,
    pub(in crate::runtime_support) status: String,
    #[serde(default)]
    pub(in crate::runtime_support) priority: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(in crate::runtime_support) struct TodoWriteRequest {
    pub(in crate::runtime_support) todos: Vec<TodoWriteItemInput>,
    #[serde(default)]
    pub(in crate::runtime_support) summary: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(in crate::runtime_support) struct ExitPlanModeRequest {
    pub(in crate::runtime_support) plan_summary: String,
    #[serde(default)]
    pub(in crate::runtime_support) steps: Vec<String>,
    #[serde(default)]
    pub(in crate::runtime_support) risks: Vec<String>,
    #[serde(default)]
    pub(in crate::runtime_support) next_action: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(in crate::runtime_support) struct AgentTaskRequest {
    pub(in crate::runtime_support) task: String,
    #[serde(default)]
    pub(in crate::runtime_support) context: Option<String>,
    #[serde(default)]
    pub(in crate::runtime_support) expected_output: Option<String>,
    #[serde(default)]
    pub(in crate::runtime_support) priority: Option<String>,
}

pub(in crate::runtime_support) trait RuntimeToolHandler:
    Send + Sync
{
    fn name(&self) -> &'static str;

    fn validate_input(&self, _call: &ToolCall) -> Result<(), ToolResult> {
        Ok(())
    }

    fn check_permissions(
        &self,
        _state: &Arc<AppState>,
        turn: &TurnContext,
        call: &ToolCall,
    ) -> Result<(), ToolResult> {
        if runtime_tool_allowed(turn, &call.name) {
            Ok(())
        } else {
            Err(ToolResult {
                status: ToolResultStatus::Failed,
                content: format!("当前角色未被授权调用工具 `{}`。", call.name),
                structured: Some(serde_json::json!({ "reason": "unauthorized" })),
            })
        }
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!("执行工具：{}", call.name)
    }

    fn safety_notes(&self, _call: &ToolCall) -> Vec<String> {
        Vec::new()
    }

    fn requires_workspace_boundary_approval(
        &self,
        _policy: &FrozenExecutionPolicy,
        _call: &ToolCall,
    ) -> bool {
        false
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        true
    }

    fn is_mutating(&self, call: &ToolCall) -> bool {
        !self.is_read_only(call)
    }

    fn is_concurrency_safe(&self, call: &ToolCall) -> bool {
        self.is_read_only(call)
    }

    fn interrupt_behavior(&self, _call: &ToolCall) -> RuntimeToolInterruptBehavior {
        RuntimeToolInterruptBehavior::Block
    }

    fn render_result_for_model(&self, result: &ToolResult) -> String {
        default_tool_result_content_for_model(result)
    }

    fn context_effect(&self, _result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        None
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a>;
}

pub(in crate::runtime_support) struct AskUserQuestionHandler;
pub(in crate::runtime_support) struct TtsSpeakHandler;
pub(in crate::runtime_support) struct VoiceCurrentHandler;
pub(in crate::runtime_support) struct FileReadHandler;
pub(in crate::runtime_support) struct FileListHandler;
pub(in crate::runtime_support) struct FileSearchHandler;
pub(in crate::runtime_support) struct FileWriteHandler;
pub(in crate::runtime_support) struct FileEditHandler;
pub(in crate::runtime_support) struct CommandRunHandler;
pub(in crate::runtime_support) struct WebFetchHandler;
pub(in crate::runtime_support) struct WebSearchHandler;
pub(in crate::runtime_support) struct SessionListHandler;
pub(in crate::runtime_support) struct SessionReadHandler;
pub(in crate::runtime_support) struct ToolResultReadHandler;
pub(in crate::runtime_support) struct SessionCompactHandler;
pub(in crate::runtime_support) struct ModelInfoHandler;
pub(in crate::runtime_support) struct PersonaInfoHandler;
pub(in crate::runtime_support) struct PersonaSwitchHandler;
pub(in crate::runtime_support) struct McpListResourcesHandler;
pub(in crate::runtime_support) struct McpListResourceTemplatesHandler;
pub(in crate::runtime_support) struct McpReadResourceHandler;
pub(in crate::runtime_support) struct TodoWriteHandler;
pub(in crate::runtime_support) struct EnterPlanModeHandler;
pub(in crate::runtime_support) struct ExitPlanModeHandler;
pub(in crate::runtime_support) struct SendUserMessageHandler;
pub(in crate::runtime_support) struct BriefHandler;
pub(in crate::runtime_support) struct SkillHandler {
    name: &'static str,
}
pub(in crate::runtime_support) struct CreateSkillHandler;
pub(in crate::runtime_support) struct AgentHandler;
pub(in crate::runtime_support) struct TaskStopHandler;

impl RuntimeToolHandler for TodoWriteHandler {
    fn name(&self) -> &'static str {
        "todo_write"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        parse_todo_write_request(&call.arguments).map(|_| ())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match parse_todo_write_request(&call.arguments) {
            Ok(request) => format!("更新任务清单：{} 项", request.todos.len()),
            Err(_) => "更新任务清单".to_string(),
        }
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "任务清单",
            content: result.content.clone(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_todo_write(invocation.state, invocation.turn, invocation.call).await
        })
    }
}

impl RuntimeToolHandler for EnterPlanModeHandler {
    fn name(&self) -> &'static str {
        "enter_plan_mode"
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        tool_arg_string(&call.arguments, "reason")
            .map(|reason| format!("进入计划预设：{reason}"))
            .unwrap_or_else(|| "进入计划预设。".to_string())
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "运行模式",
            content: "当前已进入计划态；只能读、查、问和写计划，不能直接执行写入或命令。"
                .to_string(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_enter_plan_mode(invocation.state, invocation.turn, invocation.call).await
        })
    }
}

impl RuntimeToolHandler for ExitPlanModeHandler {
    fn name(&self) -> &'static str {
        "exit_plan_mode"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        parse_exit_plan_mode_request(&call.arguments).map(|_| ())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match parse_exit_plan_mode_request(&call.arguments) {
            Ok(request) => format!(
                "提交计划确认：{}",
                truncate_text(request.plan_summary.trim(), 80)
            ),
            Err(_) => "提交计划确认。".to_string(),
        }
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "计划确认",
            content: result.content.clone(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_exit_plan_mode(
                invocation.state,
                invocation.tx,
                invocation.turn,
                invocation.call,
                invocation.cancel_token,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for SendUserMessageHandler {
    fn name(&self) -> &'static str {
        "send_user_message"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "message")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        let msg = call
            .arguments
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("发送阶段简报：{msg}")
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        true
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "阶段简报",
            content: result.content.clone(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_send_user_message(
                invocation.state,
                invocation.tx,
                invocation.turn,
                invocation.call,
                invocation.cancel_token,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for BriefHandler {
    fn name(&self) -> &'static str {
        "brief"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "message")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        let msg = call
            .arguments
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("发送阶段简报：{msg}")
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        true
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "阶段简报",
            content: result.content.clone(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_send_user_message(
                invocation.state,
                invocation.tx,
                invocation.turn,
                invocation.call,
                invocation.cancel_token,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for SkillHandler {
    fn name(&self) -> &'static str {
        self.name
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "skill_name")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        let name = call
            .arguments
            .get("skill_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("载入技能 `{name}`")
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        true
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(
            async move { tool_skill(invocation.state, invocation.turn, invocation.call).await },
        )
    }
}

impl RuntimeToolHandler for CreateSkillHandler {
    fn name(&self) -> &'static str {
        "create_skill"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        create_skill_draft_from_call(call).map(|_| ())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        let name = call
            .arguments
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        format!("创建持久化 Skill `{name}`")
    }

    fn safety_notes(&self, _call: &ToolCall) -> Vec<String> {
        vec!["新 Skill 会写入用户数据目录并在后续对话中持续可用。".to_string()]
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "Skill 创建结果",
            content: result.content.clone(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_create_skill(invocation.state, invocation.turn, invocation.call).await
        })
    }
}

impl RuntimeToolHandler for AgentHandler {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        parse_agent_task_request(&call.arguments).map(|_| ())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match parse_agent_task_request(&call.arguments) {
            Ok(request) => format!("登记子任务：{}", truncate_text(request.task.trim(), 80)),
            Err(_) => "登记子任务".to_string(),
        }
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        Some(RuntimeToolContextEffect::Append {
            title: "子任务",
            content: result.content.clone(),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(
            async move { tool_agent(invocation.state, invocation.turn, invocation.call).await },
        )
    }
}

impl RuntimeToolHandler for TaskStopHandler {
    fn name(&self) -> &'static str {
        "task_stop"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "reason")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        let reason = call
            .arguments
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("停止任务：{reason}")
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(
            async move { tool_task_stop(invocation.state, invocation.turn, invocation.call).await },
        )
    }
}

impl RuntimeToolHandler for AskUserQuestionHandler {
    fn name(&self) -> &'static str {
        "ask_user_question"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        parse_ask_user_question_request(&call.arguments).map(|_| ())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match parse_ask_user_question_request(&call.arguments) {
            Ok(request) => format!("向用户提问：{} 个问题", request.questions.len()),
            Err(_) => "向用户提问".to_string(),
        }
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_ask_user_question(
                invocation.state,
                invocation.tx,
                invocation.turn,
                invocation.call,
                invocation.cancel_token,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for TtsSpeakHandler {
    fn name(&self) -> &'static str {
        "tts_speak"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        for forbidden in ["voice_id", "voice_name", "speaker"] {
            if call.arguments.get(forbidden).is_some() {
                return Err(tool_failed(
                    format!("tts_speak 不接受 `{forbidden}` 参数，语音永远使用当前启用音色。"),
                    "forbidden_voice_argument",
                ));
            }
        }
        require_text_argument(call, "text")
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(
            async move { tool_tts_speak(invocation.state, invocation.turn, invocation.call).await },
        )
    }
}

impl RuntimeToolHandler for VoiceCurrentHandler {
    fn name(&self) -> &'static str {
        "voice_current"
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_voice_current(invocation.turn) })
    }
}

impl RuntimeToolHandler for FileReadHandler {
    fn name(&self) -> &'static str {
        "file_read"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "path")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "读取文件：{}",
            tool_arg_string(&call.arguments, "path").unwrap_or_else(|| "未知路径".to_string())
        )
    }

    fn requires_workspace_boundary_approval(
        &self,
        policy: &FrozenExecutionPolicy,
        call: &ToolCall,
    ) -> bool {
        tool_arg_string(&call.arguments, "path")
            .is_some_and(|path| path_requires_workspace_boundary_approval(policy, &path, false))
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_file_read(
                invocation.execution_policy,
                invocation.call,
                invocation.allow_approved_external_path,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for FileListHandler {
    fn name(&self) -> &'static str {
        "file_list"
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "列出目录：{}",
            tool_arg_string(&call.arguments, "path").unwrap_or_else(|| ".".to_string())
        )
    }

    fn requires_workspace_boundary_approval(
        &self,
        policy: &FrozenExecutionPolicy,
        call: &ToolCall,
    ) -> bool {
        let path = tool_arg_string(&call.arguments, "path").unwrap_or_else(|| ".".to_string());
        path_requires_workspace_boundary_approval(policy, &path, false)
    }

    fn render_result_for_model(&self, result: &ToolResult) -> String {
        let Some(entries) = result
            .structured
            .as_ref()
            .and_then(|value| value.get("entries"))
            .and_then(|value| value.as_array())
        else {
            return default_tool_result_content_for_model(result);
        };
        let preview = entries
            .iter()
            .take(60)
            .filter_map(|entry| {
                let path = entry.get("path").and_then(|value| value.as_str())?;
                let kind = entry
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                Some(format!("- [{kind}] {path}"))
            })
            .collect::<Vec<_>>();
        if preview.is_empty() {
            result.content.clone()
        } else {
            format!(
                "{}\n\n目录条目（供模型继续决策）：\n{}",
                result.content,
                preview.join("\n")
            )
        }
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_file_list(
                invocation.execution_policy,
                invocation.call,
                invocation.allow_approved_external_path,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for FileSearchHandler {
    fn name(&self) -> &'static str {
        "file_search"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "query")?;
        if let Some(kind) = tool_arg_string(&call.arguments, "kind")
            && !matches!(kind.as_str(), "any" | "file" | "directory")
        {
            return Err(tool_failed(
                "file_search kind 只支持 any、file、directory。",
                "invalid_kind",
            ));
        }
        if let Some(match_mode) = tool_arg_string(&call.arguments, "match")
            && !matches!(match_mode.as_str(), "name" | "path" | "content")
        {
            return Err(tool_failed(
                "file_search match 只支持 name、path、content。",
                "invalid_match",
            ));
        }
        Ok(())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "搜索目录：{}",
            tool_arg_string(&call.arguments, "base").unwrap_or_else(|| ".".to_string())
        )
    }

    fn requires_workspace_boundary_approval(
        &self,
        _policy: &FrozenExecutionPolicy,
        _call: &ToolCall,
    ) -> bool {
        // file_search 是严格只读的工作区检索能力。工作区外路径不能通过
        // full_access 或一次性审批变成可搜索范围。
        false
    }

    fn interrupt_behavior(&self, _call: &ToolCall) -> RuntimeToolInterruptBehavior {
        RuntimeToolInterruptBehavior::Cancel
    }

    fn render_result_for_model(&self, result: &ToolResult) -> String {
        let Some(results) = result
            .structured
            .as_ref()
            .and_then(|value| value.get("results"))
            .and_then(|value| value.as_array())
        else {
            return default_tool_result_content_for_model(result);
        };
        let preview = results
            .iter()
            .take(60)
            .filter_map(|entry| {
                let path = entry.get("path").and_then(|value| value.as_str())?;
                let kind = entry
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                Some(format!("- [{kind}] {path}"))
            })
            .collect::<Vec<_>>();
        if preview.is_empty() {
            result.content.clone()
        } else {
            format!(
                "{}\n\n搜索候选路径（供模型继续决策）：\n{}",
                result.content,
                preview.join("\n")
            )
        }
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(
            async move { tool_file_search(invocation.execution_policy, invocation.call).await },
        )
    }
}

impl RuntimeToolHandler for FileWriteHandler {
    fn name(&self) -> &'static str {
        "file_write"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "path")?;
        require_string_argument(call, "content", true)
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "写入文件：{}",
            tool_arg_string(&call.arguments, "path").unwrap_or_else(|| "未知路径".to_string())
        )
    }

    fn requires_workspace_boundary_approval(
        &self,
        policy: &FrozenExecutionPolicy,
        call: &ToolCall,
    ) -> bool {
        tool_arg_string(&call.arguments, "path")
            .is_some_and(|path| path_requires_workspace_boundary_approval(policy, &path, true))
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        let path = result
            .structured
            .as_ref()
            .and_then(|value| value.get("path"))
            .and_then(|value| value.as_str())
            .unwrap_or("未知路径");
        Some(RuntimeToolContextEffect::Append {
            title: "文件写入",
            content: format!(
                "文件 `{path}` 已写入成功。后续读取或引用该路径时，应以最新写入内容为准。"
            ),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_file_write(
                invocation.execution_policy,
                invocation.call,
                invocation.allow_approved_external_path,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for FileEditHandler {
    fn name(&self) -> &'static str {
        "file_edit"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "path")?;
        require_text_argument(call, "old_string")?;
        require_string_argument(call, "new_string", true)?;
        let old_string = call
            .arguments
            .get("old_string")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let new_string = call
            .arguments
            .get("new_string")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if old_string == new_string {
            Err(tool_failed(
                "old_string 与 new_string 完全相同，没有可编辑内容。",
                "no_change",
            ))
        } else {
            Ok(())
        }
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "编辑文件：{}",
            tool_arg_string(&call.arguments, "path").unwrap_or_else(|| "未知路径".to_string())
        )
    }

    fn requires_workspace_boundary_approval(
        &self,
        policy: &FrozenExecutionPolicy,
        call: &ToolCall,
    ) -> bool {
        tool_arg_string(&call.arguments, "path")
            .is_some_and(|path| path_requires_workspace_boundary_approval(policy, &path, true))
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        let path = result
            .structured
            .as_ref()
            .and_then(|value| value.get("path"))
            .and_then(|value| value.as_str())
            .unwrap_or("未知路径");
        Some(RuntimeToolContextEffect::Append {
            title: "文件编辑",
            content: format!(
                "文件 `{path}` 已编辑成功。后续读取或引用该路径时，应以最新编辑后的内容为准。"
            ),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_file_edit(
                invocation.execution_policy,
                invocation.call,
                invocation.allow_approved_external_path,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for CommandRunHandler {
    fn name(&self) -> &'static str {
        "command_run"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "command")?;
        if call.arguments.get("timeout_ms").is_some()
            && call
                .arguments
                .get("timeout_ms")
                .and_then(|value| value.as_u64())
                .is_none()
        {
            return Err(tool_failed(
                "command_run timeout_ms 必须是正整数毫秒。",
                "invalid_timeout_ms",
            ));
        }
        if call.arguments.get("audit_output").is_some()
            && call
                .arguments
                .get("audit_output")
                .and_then(|value| value.as_bool())
                .is_none()
        {
            return Err(tool_failed(
                "command_run audit_output 必须是布尔值。",
                "invalid_audit_output",
            ));
        }
        Ok(())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        let command =
            tool_arg_string(&call.arguments, "command").unwrap_or_else(|| "空命令".to_string());
        let mut summary = format!("执行命令：{}", command);
        let notes = command_risk_safety_notes(&command);
        if !notes.is_empty() {
            summary.push_str("\n风险提示：");
            for note in notes {
                summary.push_str("\n- ");
                summary.push_str(&note);
            }
        }
        if command_audit_output_requested(call) {
            summary.push_str(
                "\n审计输出：已启用；stdout/stderr 将各自最多保存 16 MiB 到 Muse 受控数据目录。",
            );
        }
        summary
    }

    fn safety_notes(&self, call: &ToolCall) -> Vec<String> {
        tool_arg_string(&call.arguments, "command")
            .map(|command| command_risk_safety_notes(&command))
            .unwrap_or_default()
    }

    fn requires_workspace_boundary_approval(
        &self,
        policy: &FrozenExecutionPolicy,
        call: &ToolCall,
    ) -> bool {
        let cwd = tool_arg_string(&call.arguments, "cwd").unwrap_or_else(|| ".".to_string());
        path_requires_workspace_boundary_approval(policy, &cwd, false)
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn interrupt_behavior(&self, _call: &ToolCall) -> RuntimeToolInterruptBehavior {
        RuntimeToolInterruptBehavior::Cancel
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_command_run(
                invocation.execution_policy,
                invocation.call,
                invocation.tx,
                invocation.allow_approved_external_path,
                invocation.cancel_token,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for WebFetchHandler {
    fn name(&self) -> &'static str {
        "web_fetch"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        let Some(url) = tool_arg_string(&call.arguments, "url") else {
            return Err(tool_failed("web_fetch 缺少 url 参数。", "missing_url"));
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(tool_failed(
                "web_fetch url 只支持 http:// 或 https://。",
                "invalid_url_scheme",
            ));
        }
        Ok(())
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "联网抓取：{}",
            tool_arg_string(&call.arguments, "url").unwrap_or_else(|| "未知 URL".to_string())
        )
    }

    fn interrupt_behavior(&self, _call: &ToolCall) -> RuntimeToolInterruptBehavior {
        RuntimeToolInterruptBehavior::Cancel
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tokio::select! {
                result = tool_web_fetch(invocation.call) => result,
                _ = wait_for_turn_cancel(invocation.cancel_token) => {
                    tool_failed(TURN_CANCELLED_MESSAGE, "turn_cancelled")
                }
            }
        })
    }
}

impl RuntimeToolHandler for WebSearchHandler {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "query")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "联网搜索：{}",
            tool_arg_string(&call.arguments, "query").unwrap_or_else(|| "空搜索词".to_string())
        )
    }

    fn interrupt_behavior(&self, _call: &ToolCall) -> RuntimeToolInterruptBehavior {
        RuntimeToolInterruptBehavior::Cancel
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tokio::select! {
                result = tool_web_search(invocation.state, invocation.call) => result,
                _ = wait_for_turn_cancel(invocation.cancel_token) => {
                    tool_failed(TURN_CANCELLED_MESSAGE, "turn_cancelled")
                }
            }
        })
    }
}

impl RuntimeToolHandler for SessionListHandler {
    fn name(&self) -> &'static str {
        "session_list"
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_session_list(invocation.state).await })
    }
}

impl RuntimeToolHandler for SessionReadHandler {
    fn name(&self) -> &'static str {
        "session_read"
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_session_read(invocation.state, invocation.call).await })
    }
}

impl RuntimeToolHandler for ToolResultReadHandler {
    fn name(&self) -> &'static str {
        "tool_result_read"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "result_id")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "读取外置工具结果：{}",
            tool_arg_string(&call.arguments, "result_id").unwrap_or_else(|| "未知结果".to_string())
        )
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_result_read(invocation.call).await })
    }
}

impl RuntimeToolHandler for SessionCompactHandler {
    fn name(&self) -> &'static str {
        "session_compact"
    }

    fn approval_summary(&self, _call: &ToolCall) -> String {
        "压缩当前会话上下文。".to_string()
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn render_result_for_model(&self, result: &ToolResult) -> String {
        result.content.clone()
    }

    fn context_effect(&self, result: &ToolResult) -> Option<RuntimeToolContextEffect> {
        if !result.is_success() {
            return None;
        }
        let summary = result
            .structured
            .as_ref()
            .and_then(|value| value.get("summary"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        Some(RuntimeToolContextEffect::Replace {
            reason: "compact_summary",
            content: format!("会话已完成压缩。后续上下文以以下摘要和最近消息为准：\n{summary}"),
        })
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_session_compact(
                invocation.state,
                invocation.provider,
                invocation.conversation,
            )
            .await
        })
    }
}

impl RuntimeToolHandler for ModelInfoHandler {
    fn name(&self) -> &'static str {
        "model_info"
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_model_info(invocation.turn) })
    }
}

impl RuntimeToolHandler for PersonaInfoHandler {
    fn name(&self) -> &'static str {
        "persona_info"
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_persona_info(invocation.state).await })
    }
}

impl RuntimeToolHandler for PersonaSwitchHandler {
    fn name(&self) -> &'static str {
        "persona_switch"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "persona_id")
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "切换角色：{}",
            tool_arg_string(&call.arguments, "persona_id")
                .unwrap_or_else(|| "未知角色".to_string())
        )
    }

    fn is_read_only(&self, _call: &ToolCall) -> bool {
        false
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move { tool_persona_switch(invocation.state, invocation.call).await })
    }
}

impl RuntimeToolHandler for McpListResourcesHandler {
    fn name(&self) -> &'static str {
        "mcp_list_resources"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        validate_mcp_server_argument(call)
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match tool_arg_string(&call.arguments, "server") {
            Some(server) => format!("列出 MCP 资源：{server}。"),
            None => "列出本地与外部 MCP 资源。".to_string(),
        }
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_mcp_list_resources(invocation.frozen_mcp_catalog, invocation.call).await
        })
    }
}

impl RuntimeToolHandler for McpListResourceTemplatesHandler {
    fn name(&self) -> &'static str {
        "mcp_list_resource_templates"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        validate_mcp_server_argument(call)
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        match tool_arg_string(&call.arguments, "server") {
            Some(server) => format!("列出 MCP resource template：{server}。"),
            None => "列出外部 MCP resource template。".to_string(),
        }
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_mcp_list_resource_templates(invocation.frozen_mcp_catalog, invocation.call).await
        })
    }
}

impl RuntimeToolHandler for McpReadResourceHandler {
    fn name(&self) -> &'static str {
        "mcp_read_resource"
    }

    fn validate_input(&self, call: &ToolCall) -> Result<(), ToolResult> {
        require_text_argument(call, "uri")?;
        validate_mcp_server_argument(call)
    }

    fn approval_summary(&self, call: &ToolCall) -> String {
        format!(
            "读取 MCP 资源：{}",
            tool_arg_string(&call.arguments, "uri").unwrap_or_else(|| "未知 URI".to_string())
        )
    }

    fn call<'a>(&'a self, invocation: RuntimeToolInvocation<'a>) -> RuntimeToolFuture<'a> {
        Box::pin(async move {
            tool_mcp_read_resource(
                invocation.state,
                invocation.frozen_mcp_catalog,
                invocation.call,
                invocation.conversation,
            )
            .await
        })
    }
}

pub(in crate::runtime_support) static ASK_USER_QUESTION_HANDLER: AskUserQuestionHandler =
    AskUserQuestionHandler;
pub(in crate::runtime_support) static TTS_SPEAK_HANDLER: TtsSpeakHandler = TtsSpeakHandler;
pub(in crate::runtime_support) static VOICE_CURRENT_HANDLER: VoiceCurrentHandler =
    VoiceCurrentHandler;
pub(in crate::runtime_support) static FILE_READ_HANDLER: FileReadHandler = FileReadHandler;
pub(in crate::runtime_support) static FILE_LIST_HANDLER: FileListHandler = FileListHandler;
pub(in crate::runtime_support) static FILE_SEARCH_HANDLER: FileSearchHandler = FileSearchHandler;
pub(in crate::runtime_support) static FILE_WRITE_HANDLER: FileWriteHandler = FileWriteHandler;
pub(in crate::runtime_support) static FILE_EDIT_HANDLER: FileEditHandler = FileEditHandler;
pub(in crate::runtime_support) static COMMAND_RUN_HANDLER: CommandRunHandler = CommandRunHandler;
pub(in crate::runtime_support) static WEB_FETCH_HANDLER: WebFetchHandler = WebFetchHandler;
pub(in crate::runtime_support) static WEB_SEARCH_HANDLER: WebSearchHandler = WebSearchHandler;
pub(in crate::runtime_support) static SESSION_LIST_HANDLER: SessionListHandler = SessionListHandler;
pub(in crate::runtime_support) static SESSION_READ_HANDLER: SessionReadHandler = SessionReadHandler;
pub(in crate::runtime_support) static TOOL_RESULT_READ_HANDLER: ToolResultReadHandler =
    ToolResultReadHandler;
pub(in crate::runtime_support) static SESSION_COMPACT_HANDLER: SessionCompactHandler =
    SessionCompactHandler;
pub(in crate::runtime_support) static MODEL_INFO_HANDLER: ModelInfoHandler = ModelInfoHandler;
pub(in crate::runtime_support) static PERSONA_INFO_HANDLER: PersonaInfoHandler = PersonaInfoHandler;
pub(in crate::runtime_support) static PERSONA_SWITCH_HANDLER: PersonaSwitchHandler =
    PersonaSwitchHandler;
pub(in crate::runtime_support) static MCP_LIST_RESOURCES_HANDLER: McpListResourcesHandler =
    McpListResourcesHandler;
pub(in crate::runtime_support) static MCP_LIST_RESOURCE_TEMPLATES_HANDLER:
    McpListResourceTemplatesHandler = McpListResourceTemplatesHandler;
pub(in crate::runtime_support) static MCP_READ_RESOURCE_HANDLER: McpReadResourceHandler =
    McpReadResourceHandler;
pub(in crate::runtime_support) static TODO_WRITE_HANDLER: TodoWriteHandler = TodoWriteHandler;
pub(in crate::runtime_support) static ENTER_PLAN_MODE_HANDLER: EnterPlanModeHandler =
    EnterPlanModeHandler;
pub(in crate::runtime_support) static EXIT_PLAN_MODE_HANDLER: ExitPlanModeHandler =
    ExitPlanModeHandler;
pub(in crate::runtime_support) static SEND_USER_MESSAGE_HANDLER: SendUserMessageHandler =
    SendUserMessageHandler;
pub(in crate::runtime_support) static BRIEF_HANDLER: BriefHandler = BriefHandler;
pub(in crate::runtime_support) static LOAD_SKILL_HANDLER: SkillHandler =
    SkillHandler { name: "load_skill" };
pub(in crate::runtime_support) static USE_SKILL_HANDLER: SkillHandler =
    SkillHandler { name: "use_skill" };
pub(in crate::runtime_support) static SKILL_HANDLER: SkillHandler = SkillHandler { name: "skill" };
pub(in crate::runtime_support) static CREATE_SKILL_HANDLER: CreateSkillHandler = CreateSkillHandler;
pub(in crate::runtime_support) static AGENT_HANDLER: AgentHandler = AgentHandler;
pub(in crate::runtime_support) static TASK_STOP_HANDLER: TaskStopHandler = TaskStopHandler;

pub(in crate::runtime_support) static RUNTIME_TOOL_HANDLERS: &[&dyn RuntimeToolHandler] = &[
    &TODO_WRITE_HANDLER,
    &ENTER_PLAN_MODE_HANDLER,
    &EXIT_PLAN_MODE_HANDLER,
    &SEND_USER_MESSAGE_HANDLER,
    &BRIEF_HANDLER,
    &LOAD_SKILL_HANDLER,
    &USE_SKILL_HANDLER,
    &SKILL_HANDLER,
    &CREATE_SKILL_HANDLER,
    &AGENT_HANDLER,
    &TASK_STOP_HANDLER,
    &ASK_USER_QUESTION_HANDLER,
    &TTS_SPEAK_HANDLER,
    &VOICE_CURRENT_HANDLER,
    &FILE_READ_HANDLER,
    &FILE_LIST_HANDLER,
    &FILE_SEARCH_HANDLER,
    &FILE_WRITE_HANDLER,
    &FILE_EDIT_HANDLER,
    &COMMAND_RUN_HANDLER,
    &WEB_FETCH_HANDLER,
    &WEB_SEARCH_HANDLER,
    &SESSION_LIST_HANDLER,
    &SESSION_READ_HANDLER,
    &TOOL_RESULT_READ_HANDLER,
    &SESSION_COMPACT_HANDLER,
    &MODEL_INFO_HANDLER,
    &PERSONA_INFO_HANDLER,
    &PERSONA_SWITCH_HANDLER,
    &MCP_LIST_RESOURCES_HANDLER,
    &MCP_LIST_RESOURCE_TEMPLATES_HANDLER,
    &MCP_READ_RESOURCE_HANDLER,
    &MEMORY_QUERY_HANDLER,
    &MEMORY_MUTATE_HANDLER,
    &MEMORY_DELETE_HANDLER,
];

pub(in crate::runtime_support) fn runtime_tool_handler(
    name: &str,
) -> Option<&'static dyn RuntimeToolHandler> {
    // 能力矩阵是运行时工具的单一准入表；处理器没有登记就不能被分发执行。
    capability(name)?;
    RUNTIME_TOOL_HANDLERS
        .iter()
        .copied()
        .find(|handler| handler.name() == name)
}

pub(in crate::runtime_support) struct RuntimeToolDispatchContext<'a> {
    execution_policy: &'a FrozenExecutionPolicy,
    frozen_mcp_catalog: &'a mcp::McpToolCatalog,
    provider: &'a Arc<dyn muse_core::model::provider::ChatModelProvider>,
    turn: &'a TurnContext,
    cancel_token: &'a RuntimeTurnCancel,
    conversation: &'a Conversation,
    memory_turn: &'a MemoryTurnState,
    approval_obtained: bool,
    approval_evidence: Option<&'a ToolApprovalEvidence>,
    allow_approved_external_path: bool,
}

pub(in crate::runtime_support) async fn dispatch_runtime_tool(
    state: &Arc<AppState>,
    tx: Option<&RuntimeSseSender>,
    context: RuntimeToolDispatchContext<'_>,
    call: &ToolCall,
) -> ToolResult {
    let RuntimeToolDispatchContext {
        execution_policy,
        frozen_mcp_catalog,
        provider,
        turn,
        cancel_token,
        conversation,
        memory_turn,
        approval_obtained,
        approval_evidence,
        allow_approved_external_path,
    } = context;
    if let Some(handler) = runtime_tool_handler(&call.name) {
        let handler_call = handler.call(RuntimeToolInvocation {
            state,
            tx,
            execution_policy,
            frozen_mcp_catalog,
            provider,
            turn,
            call,
            cancel_token,
            conversation,
            memory_turn,
            approval_obtained,
            approval_evidence,
            allow_approved_external_path,
        });
        tokio::pin!(handler_call);
        return tokio::select! {
            biased;
            result = &mut handler_call => result,
            _ = wait_for_turn_cancel(cancel_token) => {
                tool_failed(TURN_CANCELLED_MESSAGE, "turn_cancelled")
            }
        };
    }

    if let Some(tool) = frozen_mcp_catalog.find_tool(&call.name) {
        let external_call = frozen_mcp_catalog.call_tool(&tool, call.arguments.clone());
        tokio::pin!(external_call);
        let result = tokio::select! {
            result = &mut external_call => result,
            _ = wait_for_turn_cancel(cancel_token) => {
                return tool_failed(TURN_CANCELLED_MESSAGE, "turn_cancelled");
            }
        };
        return match result {
            Ok(result) => ToolResult {
                status: if result.is_error {
                    ToolResultStatus::Failed
                } else {
                    ToolResultStatus::Success
                },
                content: result.content,
                structured: Some(result.structured),
            },
            Err(err) => {
                let diagnostic = mcp::structured_mcp_error(err);
                ToolResult {
                    status: ToolResultStatus::Failed,
                    content: format!(
                        "外部 MCP 工具 `{}` 执行失败：{}",
                        call.name, diagnostic.message
                    ),
                    structured: Some(serde_json::json!({
                        "reason": diagnostic.code,
                        "kind": diagnostic.kind,
                        "tool": call.name,
                        "message": diagnostic.message,
                        "retryable": diagnostic.retryable,
                        "alternatives": diagnostic.alternatives
                    })),
                }
            }
        };
    }

    state
        .tools
        .execute_authorized(Some(&turn.tool_policy), &call.name, call.arguments.clone())
}
