//! Handler 共享实现，后续子模块通过本模块复用统一状态与安全边界。

use axum::{
    Json,
    body::Body,
    extract::{
        Extension, Multipart, Path, Query, State,
        ws::{Message as WsMsg, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response, Sse},
};
use futures::StreamExt;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path as StdPath, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::dto::*;
use crate::error::{
    bad_request, internal_error, model_catalog_error_response, persona_card_error_response,
    persona_store_error_response, visual_pack_store_error_response, voice_error_response,
};
use crate::runtime::{
    RuntimeEventEmitter, RuntimeSseSender, RuntimeTurnOutcome, runtime_event_payload,
};
use crate::runtime_tools::{RuntimeToolContextEffect, RuntimeToolInterruptBehavior};
use crate::security::{LocalApiSecurity, RuntimeHealthResponse, WsTicketResponse};
use crate::state::{
    AppState, ChatRequestRegistryError, build_chat_provider,
    build_runtime_system_prompt_with_mode_state, effective_tts_config, missing_chat_provider_error,
    next_runtime_session_id, rebuild_speech_provider_from_state, rebuild_tts_provider_from_state,
};
use muse_core::domain::conversation::{Conversation, Message, Role};
use muse_core::domain::mcp;
use muse_core::domain::persona::Persona;
use muse_core::domain::persona::character::card::PersonaCard;
use muse_core::domain::persona::character::store::{PersonaStore, PersonaStoreError};
use muse_core::domain::persona::visual::VisualPack;
use muse_core::domain::protocol::{RuntimeEvent, RuntimeOp};
use muse_core::domain::runtime::{RuntimeModeState, RuntimeTodoItem, ToolPreset};
use muse_core::domain::tool::{
    ToolCall, ToolDef, ToolRegistry, ToolResult, ToolResultStatus, ToolRisk,
};
use muse_core::domain::turn::TurnContext;
use muse_core::domain::usage::{
    ProviderTokenUsage, RuntimeContextSegment, RuntimeContextSnapshot, RuntimeTokenUsage,
    RuntimeUsageStore, TokenUsageSource,
};
use muse_core::model::config::{
    ModelsConfig, SpeechRecognitionConfig, TtsConfig, VoiceInputConfig,
};
use muse_core::model::profile::{model_capability_defaults, provider_profile_for_identity};
use muse_core::model::provider::ChatStreamEvent;
use muse_core::model::vendor::{
    ProviderSupportError, fetch_provider_balance, provider_support_capabilities,
};
use muse_runtime::coordinator::{RuntimeCoordinatorError, RuntimePhase, TurnCancellation};
use muse_runtime::interactions::{
    ApprovalDecision, InteractionResolveError, PendingApproval, PendingUserQuestion,
    UserQuestionDecision,
};
use muse_runtime::{FrozenExecutionPolicy, TurnBudget, TurnSnapshot};

struct SanitizedAssistantReply {
    content: String,
}

struct TtsRequestContext {
    effective_tts: TtsConfig,
}

static RUNTIME_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
const TOOL_APPROVAL_TIMEOUT_SECS: u64 = 300;
const USER_QUESTION_TIMEOUT_SECS: u64 = 300;
const COMMAND_RUN_TIMEOUT_MS: u64 = 120_000;
const COMMAND_RUN_MAX_TIMEOUT_MS: u64 = 120_000;
const COMMAND_OUTPUT_MEMORY_LIMIT_BYTES: usize = 1024 * 1024;
const COMMAND_OUTPUT_SSE_LIMIT_BYTES: usize = 1024 * 1024;
const COMMAND_AUDIT_OUTPUT_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const COMMAND_AUDIT_CHANNEL_CAPACITY: usize = 32;
const COMMAND_TERMINATE_GRACE_MS: u64 = 800;
const COMMAND_READER_CLEANUP_TIMEOUT_MS: u64 = 2_000;
const COMMAND_AUDIT_READER_CLEANUP_TIMEOUT_MS: u64 = 10_000;
const COMMAND_AUDIT_RESOURCE_PREFIX: &str = "muse://command-audit/";
const MCP_TOOL_CATALOG_TTL_MS: u64 = 30_000;
const DEFAULT_CONVERSATION_ID: &str = "default";
const TURN_CANCELLED_MESSAGE: &str = "当前 turn 已由用户取消。";
const PERSONA_REQUIRED_ERROR: &str =
    "persona_required：当前没有激活角色，请先创建、导入或选择角色。";
const RUNTIME_FACT_SNAPSHOT_MAX_ATTEMPTS: usize = 8;
const MAX_SKILL_DOCUMENT_BYTES: u64 = 128 * 1024;
const MAX_PERSONA_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const MAX_SPEECH_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
const DEFAULT_PERSONA_THEME_COLOR: &str = "#d8596f";

#[derive(Clone)]
struct RuntimeTurnCancel {
    cancellation: TurnCancellation,
}

impl RuntimeTurnCancel {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    async fn cancelled(&self) {
        let mut cancellation = self.cancellation.clone();
        cancellation.cancelled().await;
    }
}

struct ActiveTurnGuard {
    state: Arc<AppState>,
    turn_id: String,
    runtime_lease: Option<muse_runtime::coordinator::TurnLease>,
}

#[derive(Clone)]
enum PendingInteractionKind {
    Approval(String),
    UserQuestion(String),
}

/// pending 交互登记的生命周期守卫。
///
/// 整回合硬期限通过丢弃 Future 中止等待，因此不能只依赖等待分支里的显式
/// `remove`。守卫在正常返回、事件发送失败和 Future 被丢弃时都会按 turn/request
/// 精确清理登记，避免运行时已经 Idle 但旧卡片仍可提交。
struct PendingInteractionGuard {
    state: Arc<AppState>,
    turn_id: String,
    kind: PendingInteractionKind,
}

struct ClientDisconnectGuard(Option<tokio::task::JoinHandle<()>>);

impl Drop for ClientDisconnectGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        cleanup_pending_interactions_for_turn_best_effort(&self.state, &self.turn_id);
        if let Some(lease) = self.runtime_lease.take() {
            let _ = lease.finish();
        }
    }
}

impl PendingInteractionGuard {
    fn approval(state: &Arc<AppState>, turn_id: &str, approval_id: &str) -> Self {
        Self {
            state: state.clone(),
            turn_id: turn_id.to_string(),
            kind: PendingInteractionKind::Approval(approval_id.to_string()),
        }
    }

    fn user_question(state: &Arc<AppState>, turn_id: &str, request_id: &str) -> Self {
        Self {
            state: state.clone(),
            turn_id: turn_id.to_string(),
            kind: PendingInteractionKind::UserQuestion(request_id.to_string()),
        }
    }
}

impl Drop for PendingInteractionGuard {
    fn drop(&mut self) {
        cleanup_pending_interaction_best_effort(&self.state, &self.turn_id, self.kind.clone());
    }
}

fn register_active_turn(
    state: &Arc<AppState>,
    turn_id: &str,
    runtime_lease: muse_runtime::coordinator::TurnLease,
) -> Result<(RuntimeTurnCancel, ActiveTurnGuard), String> {
    if runtime_lease.turn_id() != turn_id {
        return Err("运行时 turn lease 与当前 turn 不一致。".to_string());
    }
    let cancellation = runtime_lease.cancellation();
    Ok((
        RuntimeTurnCancel { cancellation },
        ActiveTurnGuard {
            state: state.clone(),
            turn_id: turn_id.to_string(),
            runtime_lease: Some(runtime_lease),
        },
    ))
}

fn cleanup_pending_interaction_best_effort(
    state: &Arc<AppState>,
    turn_id: &str,
    kind: PendingInteractionKind,
) {
    let cleaned_synchronously = match &kind {
        PendingInteractionKind::Approval(approval_id) => state
            .runtime_service
            .try_remove_pending_approval(approval_id, turn_id),
        PendingInteractionKind::UserQuestion(request_id) => state
            .runtime_service
            .try_remove_pending_user_question(request_id, turn_id),
    };
    if cleaned_synchronously {
        return;
    }

    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(turn_id, "运行时已停止，无法异步清理 pending 交互登记");
        return;
    };
    let state = state.clone();
    let turn_id = turn_id.to_string();
    runtime.spawn(async move {
        match kind {
            PendingInteractionKind::Approval(approval_id) => {
                state
                    .runtime_service
                    .remove_pending_approval(&approval_id, &turn_id)
                    .await;
            }
            PendingInteractionKind::UserQuestion(request_id) => {
                state
                    .runtime_service
                    .remove_pending_user_question(&request_id, &turn_id)
                    .await;
            }
        }
    });
}

fn cleanup_pending_interactions_for_turn_best_effort(state: &Arc<AppState>, turn_id: &str) {
    if state
        .runtime_service
        .try_remove_pending_interactions_for_turn(turn_id)
    {
        return;
    }

    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            turn_id,
            "运行时已停止，无法异步清理当前 turn 的 pending 交互"
        );
        return;
    };
    let state = state.clone();
    let turn_id = turn_id.to_string();
    runtime.spawn(async move {
        state
            .runtime_service
            .remove_pending_interactions_for_turn(&turn_id)
            .await;
    });
}

fn mark_active_turn_external_effects(state: &Arc<AppState>, turn_id: &str) -> Result<(), String> {
    state
        .runtime_service
        .mark_external_effect(turn_id)
        .map_err(|err| format!("标记回合外部副作用边界失败：{err}"))
}

fn active_turn_had_external_effects(state: &Arc<AppState>, turn_id: &str) -> bool {
    state
        .runtime_service
        .active_turn_had_external_effects(turn_id)
        .unwrap_or(false)
}

fn request_active_turn_cancel(state: &Arc<AppState>, turn_id: &str) {
    let _ = state.runtime_service.cancel_turn(turn_id);
}

fn bind_client_disconnect_to_turn(
    state: Arc<AppState>,
    turn_id: String,
    emitter: RuntimeEventEmitter,
) -> ClientDisconnectGuard {
    if !emitter.is_streaming() {
        return ClientDisconnectGuard(None);
    }
    ClientDisconnectGuard(Some(tokio::spawn(async move {
        emitter.wait_closed().await;
        request_active_turn_cancel(&state, &turn_id);
    })))
}

async fn cancel_active_turn(
    state: &Arc<AppState>,
    turn_id: &str,
) -> Result<RuntimeTurnOutcome, String> {
    let snapshot = state
        .runtime_service
        .snapshot()
        .map_err(|err| err.to_string())?;
    let Some(active_turn_id) = snapshot.turn_id else {
        return Err("当前没有运行中的 turn。".to_string());
    };
    if active_turn_id != turn_id {
        return Err(format!(
            "当前运行中的 turn 是 `{}`，无法取消 `{turn_id}`。",
            active_turn_id
        ));
    }

    state
        .runtime_service
        .cancel_turn(turn_id)
        .map_err(|err| err.to_string())?;
    state
        .runtime_service
        .cancel_pending_interactions_for_turn(turn_id, "用户取消当前 turn。")
        .await;
    append_transcript_record(
        state,
        "turn_cancel_requested",
        serde_json::json!({
            "conversation_id": active_conversation_id(state),
            "turn_id": turn_id,
            "reason": "user_cancelled",
        }),
    )
    .await?;
    Ok(RuntimeTurnOutcome {
        reply: TURN_CANCELLED_MESSAGE.to_string(),
    })
}

async fn wait_for_turn_cancel(cancel_token: &RuntimeTurnCancel) {
    let mut cancellation = cancel_token.cancellation.clone();
    cancellation.cancelled().await;
}

fn active_conversation_id(state: &Arc<AppState>) -> String {
    state
        .runtime_service
        .active_conversation_id()
        .unwrap_or_else(|_| DEFAULT_CONVERSATION_ID.to_string())
}

/// 返回当前活动角色，作为所有会产生新运行时事实入口的统一能力门禁。
async fn require_active_persona(state: &Arc<AppState>) -> Result<Persona, String> {
    state
        .personas
        .lock()
        .await
        .active_persona()
        .cloned()
        .ok_or_else(|| PERSONA_REQUIRED_ERROR.to_string())
}

fn persona_required_response() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::CONFLICT,
        Json(ErrorResponse {
            error: PERSONA_REQUIRED_ERROR.to_string(),
        }),
    )
}

fn runtime_turn_prepare_error_response(
    error: RuntimeCoordinatorError,
) -> (StatusCode, Json<ErrorResponse>) {
    match error {
        RuntimeCoordinatorError::Busy { .. }
        | RuntimeCoordinatorError::ExclusiveOperationBusy { .. } => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("runtime_busy：{error}"),
            }),
        ),
        _ => internal_error(error.to_string()),
    }
}

async fn require_active_persona_http(
    state: &Arc<AppState>,
) -> Result<Persona, (StatusCode, Json<ErrorResponse>)> {
    require_active_persona(state)
        .await
        .map_err(|_| persona_required_response())
}

fn set_active_conversation_id(state: &Arc<AppState>, conversation_id: &str) -> Result<(), String> {
    state
        .runtime_service
        .set_active_conversation_id(conversation_id)
        .map_err(|err| err.to_string())
}

fn acquire_runtime_idle_lease(
    state: &Arc<AppState>,
    operation: &str,
) -> Result<muse_runtime::coordinator::IdleLease, (StatusCode, Json<ErrorResponse>)> {
    state
        .runtime_service
        .acquire_idle_lease(operation)
        .map_err(|err| {
            (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!("runtime_busy：{err}"),
                }),
            )
        })
}

fn finish_runtime_idle_lease(
    lease: muse_runtime::coordinator::IdleLease,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    lease
        .finish()
        .map_err(|error| internal_error(error.to_string()))
}

pub(super) fn current_runtime_mode_state(state: &Arc<AppState>) -> RuntimeModeState {
    state.runtime_service.runtime_mode().unwrap_or_default()
}

fn set_runtime_mode_state(
    state: &Arc<AppState>,
    next: RuntimeModeState,
) -> Result<RuntimeModeState, String> {
    state
        .runtime_service
        .set_runtime_mode(next)
        .map_err(|err| err.to_string())?;
    Ok(next)
}

fn runtime_mode_notice(mode_state: RuntimeModeState) -> &'static str {
    match mode_state.tool_preset() {
        ToolPreset::Daily => "已进入默认工作态。",
        ToolPreset::FocusPlan => "已进入计划态。",
        ToolPreset::FocusBuild => "已恢复默认工作态。",
    }
}

fn parse_runtime_mode_state(
    mode: &str,
    focus_phase: Option<&str>,
) -> Result<RuntimeModeState, String> {
    match mode.trim() {
        // 旧客户端的 daily 请求迁移到默认工作态，不再恢复已移除的产品模式。
        "daily" => Ok(RuntimeModeState::focus_build()),
        "focus" => match focus_phase.unwrap_or("build").trim() {
            "plan" => Ok(RuntimeModeState::focus_plan()),
            "build" | "" => Ok(RuntimeModeState::focus_build()),
            _ => Err("未知专注阶段。".to_string()),
        },
        _ => Err("未知运行模式。".to_string()),
    }
}

pub(super) fn runtime_mode_response(mode_state: RuntimeModeState) -> RuntimeModeResponse {
    RuntimeModeResponse {
        mode: mode_state.mode.as_str().to_string(),
        focus_phase: mode_state.focus_phase.as_str().to_string(),
        tool_preset: mode_state.tool_preset().as_str().to_string(),
        status: runtime_mode_notice(mode_state).to_string(),
    }
}

pub(super) async fn current_runtime_todos(state: &Arc<AppState>) -> Vec<RuntimeTodoItem> {
    state.runtime_service.runtime_todos().await
}

async fn replace_runtime_todos(state: &Arc<AppState>, todos: Vec<RuntimeTodoItem>) {
    state.runtime_service.replace_runtime_todos(todos).await;
}

struct ConversationRuntime {
    state: Arc<AppState>,
    voice_enabled: bool,
}

/// 已在 HTTP 成功响应前完成角色校验和 turn 占位的用户回合。
struct PreparedUserTurn {
    active_persona: Persona,
    budget_started_at: Instant,
    conversation_id: String,
    turn_id: String,
    runtime_lease: muse_runtime::coordinator::TurnLease,
}

impl ConversationRuntime {
    fn new(state: Arc<AppState>, voice_enabled: bool) -> Self {
        Self {
            state,
            voice_enabled,
        }
    }

    async fn submit(
        &self,
        op: RuntimeOp,
        emitter: RuntimeEventEmitter,
    ) -> Result<RuntimeTurnOutcome, String> {
        match op {
            RuntimeOp::UserTurn { message } => self.run_user_turn(message, emitter).await,
            RuntimeOp::CancelTurn { turn_id } => cancel_active_turn(&self.state, &turn_id).await,
            RuntimeOp::ApproveTool { approval_id } => {
                let turn_id = self
                    .state
                    .runtime_service
                    .snapshot()
                    .map_err(|err| err.to_string())?
                    .turn_id
                    .ok_or_else(|| "当前没有等待审批的回合。".to_string())?;
                let decision = resolve_runtime_approval_command(
                    self.state.clone(),
                    turn_id,
                    approval_id,
                    true,
                    None,
                )
                .await
                .map_err(|(_, Json(error))| error.error)?;
                Ok(RuntimeTurnOutcome {
                    reply: format!("审批 `{}` 已通过。", decision.approval_id),
                })
            }
            RuntimeOp::RejectTool {
                approval_id,
                reason,
            } => {
                let turn_id = self
                    .state
                    .runtime_service
                    .snapshot()
                    .map_err(|err| err.to_string())?
                    .turn_id
                    .ok_or_else(|| "当前没有等待审批的回合。".to_string())?;
                let decision = resolve_runtime_approval_command(
                    self.state.clone(),
                    turn_id,
                    approval_id,
                    false,
                    reason,
                )
                .await
                .map_err(|(_, Json(error))| error.error)?;
                Ok(RuntimeTurnOutcome {
                    reply: format!("审批 `{}` 已拒绝。", decision.approval_id),
                })
            }
            RuntimeOp::ResumeSession { conversation_id } => {
                self.resume_session(conversation_id, emitter).await
            }
            RuntimeOp::ForkSession {
                source_conversation_id,
                before_user_message_index,
            } => {
                self.fork_session(source_conversation_id, before_user_message_index, emitter)
                    .await
            }
        }
    }

    async fn resume_session(
        &self,
        conversation_id: String,
        emitter: RuntimeEventEmitter,
    ) -> Result<RuntimeTurnOutcome, String> {
        emitter
            .emit(RuntimeEvent::Status {
                phase: "session_resuming".to_string(),
                message: "正在从本地 transcript 恢复会话。".to_string(),
                detail: Some(
                    "恢复逻辑会重放用户消息、assistant 回复、工具调用和工具结果。".to_string(),
                ),
                state: "active".to_string(),
            })
            .await
            .map_err(|_| "客户端连接已断开。".to_string())?;

        let (conversation, stats) =
            load_conversation_from_runtime_transcript(&self.state, &conversation_id, None).await?;
        append_transcript_record(
            &self.state,
            "session_resumed",
            serde_json::json!({
                "conversation_id": conversation_id,
                "restored_messages": stats.restored_messages,
                "records": stats.records,
                "skipped_records": stats.skipped_records,
            }),
        )
        .await?;
        {
            let mut conv = self.state.runtime_service.lock_conversation().await;
            *conv = conversation;
        }
        replace_runtime_todos(&self.state, stats.latest_todos.clone().unwrap_or_default()).await;
        set_active_conversation_id(&self.state, &conversation_id)?;

        let reply = format!(
            "已恢复会话：重放 {} 条 transcript 记录，恢复 {} 条上下文消息。",
            stats.records, stats.restored_messages
        );
        emitter
            .emit(RuntimeEvent::Status {
                phase: "session_resumed".to_string(),
                message: reply.clone(),
                detail: if stats.skipped_records > 0 {
                    Some(format!("已跳过 {} 条不可恢复记录。", stats.skipped_records))
                } else {
                    None
                },
                state: "completed".to_string(),
            })
            .await
            .ok();
        emitter.emit(RuntimeEvent::Done).await.ok();

        Ok(RuntimeTurnOutcome { reply })
    }

    async fn fork_session(
        &self,
        source_conversation_id: String,
        before_user_message_index: Option<usize>,
        emitter: RuntimeEventEmitter,
    ) -> Result<RuntimeTurnOutcome, String> {
        emitter
            .emit(RuntimeEvent::Status {
                phase: "session_forking".to_string(),
                message: "正在从本地 transcript 分叉会话。".to_string(),
                detail: Some("分叉会按来源会话重放历史，并可在指定用户消息前截断。".to_string()),
                state: "active".to_string(),
            })
            .await
            .map_err(|_| "客户端连接已断开。".to_string())?;

        let new_conversation_id = next_runtime_session_id();
        let active_persona = require_active_persona(&self.state).await?;
        let (conversation, stats) = load_conversation_from_runtime_transcript(
            &self.state,
            &source_conversation_id,
            before_user_message_index,
        )
        .await?;
        let inherited_todos = stats.latest_todos.clone().unwrap_or_default();
        self.state
            .runtime_service
            .session_repository()
            .await
            .map_err(|error| error.to_string())?
            .initialize_fork(
                &new_conversation_id,
                &source_conversation_id,
                &active_persona.id,
                &active_persona.name,
                &active_persona.version,
            )
            .await
            .map_err(|error| error.to_string())?;
        persist_fork_snapshot(
            &self.state,
            &new_conversation_id,
            &source_conversation_id,
            before_user_message_index,
            &conversation,
            &inherited_todos,
        )
        .await?;
        append_transcript_record(
            &self.state,
            "session_forked",
            serde_json::json!({
                "conversation_id": new_conversation_id,
                "source_conversation_id": source_conversation_id,
                "before_user_message_index": before_user_message_index,
                "restored_messages": stats.restored_messages,
                "records": stats.records,
                "skipped_records": stats.skipped_records,
            }),
        )
        .await?;
        if !inherited_todos.is_empty() {
            append_transcript_record(
                &self.state,
                "todo_state",
                serde_json::json!({
                    "conversation_id": new_conversation_id,
                    "source_conversation_id": source_conversation_id,
                    "todos": inherited_todos,
                    "summary": "从分叉来源继承任务清单。",
                    "updated_at": chrono::Utc::now().to_rfc3339(),
                }),
            )
            .await?;
        }
        {
            let mut conv = self.state.runtime_service.lock_conversation().await;
            *conv = conversation;
        }
        replace_runtime_todos(&self.state, inherited_todos.clone()).await;
        set_active_conversation_id(&self.state, &new_conversation_id)?;

        let reply = format!(
            "已分叉会话：从来源会话重放 {} 条 transcript 记录，恢复 {} 条上下文消息，新会话 `{}` 已激活。",
            stats.records, stats.restored_messages, new_conversation_id
        );
        emitter
            .emit(RuntimeEvent::Status {
                phase: "session_forked".to_string(),
                message: reply.clone(),
                detail: None,
                state: "completed".to_string(),
            })
            .await
            .ok();
        emitter.emit(RuntimeEvent::Done).await.ok();

        Ok(RuntimeTurnOutcome { reply })
    }

    async fn run_user_turn(
        &self,
        message: String,
        emitter: RuntimeEventEmitter,
    ) -> Result<RuntimeTurnOutcome, String> {
        let prepared = self.prepare_user_turn().await?;
        self.run_prepared_user_turn(message, None, emitter, prepared)
            .await
    }

    /// 校验角色并原子占用运行时；调用方可在 HTTP 200 前完成此步骤。
    async fn prepare_user_turn(&self) -> Result<PreparedUserTurn, String> {
        let active_persona = require_active_persona(&self.state).await?;
        self.occupy_user_turn(active_persona)
            .map_err(|error| error.to_string())
    }

    fn occupy_user_turn(
        &self,
        active_persona: Persona,
    ) -> Result<PreparedUserTurn, RuntimeCoordinatorError> {
        let budget_started_at = Instant::now();
        let conversation_id = active_conversation_id(&self.state);
        let turn_id = next_runtime_id("turn");
        let runtime_lease = self
            .state
            .runtime_service
            .begin_turn(turn_id.clone(), conversation_id.clone())?;
        Ok(PreparedUserTurn {
            active_persona,
            budget_started_at,
            conversation_id,
            turn_id,
            runtime_lease,
        })
    }

    async fn run_prepared_user_turn(
        &self,
        message: String,
        selected_skill: Option<String>,
        emitter: RuntimeEventEmitter,
        prepared: PreparedUserTurn,
    ) -> Result<RuntimeTurnOutcome, String> {
        let PreparedUserTurn {
            active_persona,
            budget_started_at,
            conversation_id,
            turn_id,
            runtime_lease,
        } = prepared;
        let (cancel_token, _active_turn_guard) =
            register_active_turn(&self.state, &turn_id, runtime_lease)?;
        let _client_disconnect_guard =
            bind_client_disconnect_to_turn(self.state.clone(), turn_id.clone(), emitter.clone());
        let mode_state = current_runtime_mode_state(&self.state);
        let binding_result = async {
            let repository = self
                .state
                .runtime_service
                .session_repository()
                .await
                .map_err(|error| error.to_string())?;
            repository
                .ensure_persona_binding(
                    &conversation_id,
                    &active_persona.id,
                    &active_persona.name,
                    &active_persona.version,
                )
                .await
                .map_err(|error| error.to_string())?;
            repository
                .set_workspace_state(&active_persona.id, &conversation_id)
                .map_err(|error| error.to_string())
        }
        .await;
        if let Err(message) = binding_result {
            return Err(abort_preparing_turn(
                &self.state,
                &emitter,
                &conversation_id,
                &turn_id,
                message,
            )
            .await);
        }
        let preparation_remaining = TurnBudget::default()
            .max_duration
            .checked_sub(budget_started_at.elapsed())
            .unwrap_or(Duration::ZERO);
        let frozen_mcp_catalog = match tokio::select! {
            _ = cancel_token.cancelled() => Err(TURN_CANCELLED_MESSAGE.to_string()),
            result = tokio::time::timeout(
                preparation_remaining,
                refresh_mcp_tool_catalog_if_needed(&self.state, Some(&active_persona)),
            ) => result.map_err(|_| "回合准备阶段超过 15 分钟硬期限。".to_string()),
        } {
            Ok(catalog) => catalog,
            Err(message) => {
                return Err(abort_preparing_turn(
                    &self.state,
                    &emitter,
                    &conversation_id,
                    &turn_id,
                    message,
                )
                .await);
            }
        };
        let (skill_catalog, omitted_skill_count) =
            frozen_runtime_skill_catalog(&self.state, Some(&active_persona)).await;
        let required_skill_tools =
            selected_builtin_skill_required_tools(&skill_catalog, selected_skill.as_deref());
        let mut full_tool_defs = runtime_frozen_tool_defs_for_policy_with_catalog(
            &self.state,
            Some(&active_persona),
            &frozen_mcp_catalog,
        );
        let skill_tool_ids = match grant_selected_skill_required_tools(
            &self.state,
            Some(&active_persona),
            &mut full_tool_defs,
            &required_skill_tools,
            mode_state.tool_preset(),
        ) {
            Ok(tool_ids) => tool_ids,
            Err(message) => {
                return Err(abort_preparing_turn(
                    &self.state,
                    &emitter,
                    &conversation_id,
                    &turn_id,
                    message,
                )
                .await);
            }
        };
        let tool_defs = visible_tool_definitions_for_turn(
            &full_tool_defs,
            mode_state.tool_preset(),
            &skill_tool_ids,
        );
        let system_prompt = build_runtime_system_prompt_with_mode_state(
            &self.state.config,
            Some(&active_persona),
            &tool_defs,
            mode_state,
        );
        let frozen_runtime = match build_turn_context(
            &self.state,
            conversation_id.clone(),
            turn_id.clone(),
            Some(&active_persona),
            self.voice_enabled,
            system_prompt.clone(),
            FrozenTurnToolCatalog {
                definitions: &full_tool_defs,
                mcp: Some(&frozen_mcp_catalog),
                skills: Some(FrozenTurnSkillCatalog {
                    entries: &skill_catalog,
                    omitted_count: omitted_skill_count,
                }),
            },
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                return Err(abort_preparing_turn(
                    &self.state,
                    &emitter,
                    &conversation_id,
                    &turn_id,
                    error.to_string(),
                )
                .await);
            }
        };
        let mut turn_context = frozen_runtime.context;
        turn_context.runtime_policy.skill_tool_ids = skill_tool_ids.clone();
        let provider_snapshot = frozen_runtime.provider;
        let activated_skill_prompt = if let Some(skill_name) = selected_skill.as_deref() {
            match activate_selected_skill(&self.state, &turn_context, skill_name).await {
                Ok((prompt, activated_skill)) => {
                    turn_context.system_prompt.push_str(&prompt);
                    turn_context.runtime_policy.activated_skill = Some(activated_skill);
                    Some(prompt)
                }
                Err(message) => {
                    return Err(abort_preparing_turn(
                        &self.state,
                        &emitter,
                        &conversation_id,
                        &turn_id,
                        message,
                    )
                    .await);
                }
            }
        } else {
            None
        };
        let system_prompt = turn_context.system_prompt.clone();
        let frozen_execution_policy = self
            .state
            .runtime_service
            .execution_policy()
            .map_err(|err| err.to_string())?;
        let mut turn_snapshot = TurnSnapshot::with_budget_started_at(
            turn_context.clone(),
            full_tool_defs,
            TurnBudget::default(),
            budget_started_at,
        )
        .with_execution_policy(frozen_execution_policy);
        turn_snapshot.replace_visible_tool_definitions(tool_defs.clone())?;
        self.state
            .runtime_service
            .transition_active(&turn_context.turn_id, RuntimePhase::Running)
            .map_err(|err| err.to_string())?;

        // 回合内的 user、assistant、tool call/result 只写入私有工作副本。
        // 只有可靠写入 `turn_committed` 后，才会通过 RuntimeService 原子发布。
        let mut working_conversation = self.state.runtime_service.conversation_snapshot().await;
        update_conversation_system_prompt(&mut working_conversation, system_prompt);
        working_conversation.add_user_message(message.clone());
        let conversation_for_model = working_conversation.clone();
        if let Err(err) = append_required_turn_event(
            &self.state,
            "runtime_policy_snapshot",
            serde_json::json!({
                "conversation_id": turn_context.conversation_id,
                "turn_id": turn_context.turn_id,
                "snapshot": turn_context.runtime_policy,
            }),
        )
        .await
        {
            let message = format!("持久化运行策略快照失败：{err}");
            let message =
                rollback_runtime_turn(&self.state, &emitter, message, Some(&turn_context)).await;
            return Err(message);
        }
        if let Err(err) = append_required_turn_event(
            &self.state,
            "user",
            serde_json::json!({
                "conversation_id": turn_context.conversation_id,
                "turn_id": turn_context.turn_id,
                "content": message,
                "selected_skill": turn_context.runtime_policy.activated_skill,
            }),
        )
        .await
        {
            let message = format!("持久化用户消息失败：{err}");
            let message =
                rollback_runtime_turn(&self.state, &emitter, message, Some(&turn_context)).await;
            return Err(message);
        }
        report_transcript_failure(
            append_task_state_record(
                &self.state,
                &turn_context,
                "started",
                "本轮已开始，正在准备对话上下文。",
                None,
            )
            .await,
        );

        emitter
            .emit(RuntimeEvent::TurnStarted {
                turn_id: turn_context.turn_id.clone(),
                conversation_id: turn_context.conversation_id.clone(),
                persona_id: turn_context.persona_id.clone(),
                model_provider: turn_context.model_provider.clone(),
                model_name: turn_context.model_name.clone(),
                model_source: turn_context.model_source.clone(),
                model_fallback: turn_context.model_fallback,
                model_fallback_reason: turn_context.model_fallback_reason.clone(),
                voice_enabled: turn_context.voice_enabled,
                active_voice_id: turn_context.active_voice_id.clone(),
                voice_source: turn_context.voice_source.clone(),
                voice_fallback: turn_context.voice_fallback,
                voice_fallback_reason: turn_context.voice_fallback_reason.clone(),
                runtime_mode: mode_state.mode.as_str().to_string(),
                focus_phase: mode_state.focus_phase.as_str().to_string(),
                tool_preset: mode_state.tool_preset().as_str().to_string(),
            })
            .await
            .map_err(|_| "客户端连接已断开。".to_string())?;
        emitter
            .emit(RuntimeEvent::Status {
                phase: "queued".to_string(),
                message: "消息已送达，正在准备对话上下文。".to_string(),
                detail: None,
                state: "active".to_string(),
            })
            .await
            .map_err(|_| "客户端连接已断开。".to_string())?;
        emitter
            .emit(RuntimeEvent::Status {
                phase: "thinking".to_string(),
                message: "正在结合角色设定与对话历史分析。".to_string(),
                detail: None,
                state: "active".to_string(),
            })
            .await
            .map_err(|_| "客户端连接已断开。".to_string())?;

        if let Err(err) = conversation_for_model.validate_tool_protocol() {
            let message = format!("会话工具协议校验失败：{err}");
            let message =
                rollback_runtime_turn(&self.state, &emitter, message, Some(&turn_context)).await;
            return Err(message);
        }

        let frozen_context_profile = runtime_context_profile(&turn_context);
        let context_snapshot = build_runtime_context_snapshot(
            &turn_context,
            &conversation_for_model,
            frozen_context_profile,
        );
        turn_snapshot
            .consume_model_request()
            .map_err(|err| err.to_string())?;
        let mut current_model_epoch = turn_snapshot.capability_epoch();
        let initial_stream_result = match turn_snapshot
            .run_with_deadline(stream_provider_reply(
                &self.state,
                &provider_snapshot,
                &emitter,
                &conversation_for_model,
                &tool_defs,
                &cancel_token,
            ))
            .await
        {
            Ok(result) => result,
            Err(err) => {
                let message = err.to_string();
                let message =
                    rollback_runtime_turn(&self.state, &emitter, message, Some(&turn_context))
                        .await;
                return Err(message);
            }
        };
        let mut current_turn = match initial_stream_result {
            Ok(reply) => reply,
            Err(err) => {
                return Err(rollback_runtime_turn(
                    &self.state,
                    &emitter,
                    err.to_string(),
                    Some(&turn_context),
                )
                .await);
            }
        };
        emit_and_record_runtime_usage(
            &self.state,
            &emitter,
            &turn_context,
            context_snapshot,
            &current_turn,
        )
        .await;
        let mut final_reply = String::new();

        loop {
            if let Err(err) = turn_snapshot.ensure_not_expired() {
                return Err(
                    rollback_runtime_turn(&self.state, &emitter, err, Some(&turn_context)).await,
                );
            }
            let mut produced_tool_output = false;
            for item in current_turn.items {
                match item {
                    RuntimeModelItem::AssistantMessage {
                        content,
                        reasoning_content,
                    } => {
                        if content.is_empty() {
                            continue;
                        }
                        final_reply.push_str(&content);
                        working_conversation.add_assistant_message_with_reasoning(
                            content.clone(),
                            reasoning_content.clone(),
                        );
                        if let Err(err) = append_required_turn_event(
                            &self.state,
                            "assistant",
                            serde_json::json!({
                                "conversation_id": turn_context.conversation_id,
                                "turn_id": turn_context.turn_id,
                                "content": content,
                                "reasoning_content": reasoning_content,
                            }),
                        )
                        .await
                        {
                            let message = format!("持久化模型回复失败：{err}");
                            let message = rollback_runtime_turn(
                                &self.state,
                                &emitter,
                                message,
                                Some(&turn_context),
                            )
                            .await;
                            return Err(message);
                        }
                    }
                    RuntimeModelItem::ToolCall {
                        call: tool_call,
                        reasoning_content,
                    } => {
                        if cancel_token.is_cancelled() {
                            return Err(rollback_runtime_turn(
                                &self.state,
                                &emitter,
                                TURN_CANCELLED_MESSAGE.to_string(),
                                Some(&turn_context),
                            )
                            .await);
                        }
                        emitter
                            .emit(RuntimeEvent::Status {
                                phase: "tool_running".to_string(),
                                message: format!("正在执行工具：{}", tool_call.name),
                                detail: Some(
                                    "工具参数和结果会写入本地 transcript，危险工具需要审批。"
                                        .to_string(),
                                ),
                                state: "active".to_string(),
                            })
                            .await
                            .map_err(|_| "客户端连接已断开。".to_string())?;
                        report_transcript_failure(
                            append_task_state_record(
                                &self.state,
                                &turn_context,
                                "tool_running",
                                format!("正在执行工具：{}", tool_call.name),
                                Some(&tool_call.name),
                            )
                            .await,
                        );

                        working_conversation.add_assistant_tool_call_with_reasoning(
                            tool_call.call_id.clone(),
                            tool_call.name.clone(),
                            tool_call.arguments.clone(),
                            reasoning_content,
                        );

                        if let Err(err) = turn_snapshot.consume_tool_call() {
                            return Err(rollback_runtime_turn(
                                &self.state,
                                &emitter,
                                err,
                                Some(&turn_context),
                            )
                            .await);
                        }
                        let tool_execution = match turn_snapshot
                            .run_with_deadline(execute_runtime_tool(
                                &self.state,
                                emitter.tool_event_sender(),
                                RuntimeToolExecutionContext {
                                    snapshot: &turn_snapshot,
                                    frozen_mcp_catalog: &frozen_mcp_catalog,
                                    provider: &provider_snapshot,
                                    request_capability_epoch: current_model_epoch,
                                    turn: &turn_context,
                                    cancel_token: &cancel_token,
                                    conversation: &working_conversation,
                                },
                                tool_call.clone(),
                            ))
                            .await
                        {
                            Ok(result) => result,
                            Err(err) => {
                                let message = err.to_string();
                                let message = rollback_runtime_turn(
                                    &self.state,
                                    &emitter,
                                    message,
                                    Some(&turn_context),
                                )
                                .await;
                                return Err(message);
                            }
                        };
                        let result = match tool_execution {
                            Ok(result) => result,
                            Err(err) => {
                                return Err(rollback_runtime_turn(
                                    &self.state,
                                    &emitter,
                                    err,
                                    Some(&turn_context),
                                )
                                .await);
                            }
                        };
                        if cancel_token.is_cancelled() {
                            return Err(rollback_runtime_turn(
                                &self.state,
                                &emitter,
                                TURN_CANCELLED_MESSAGE.to_string(),
                                Some(&turn_context),
                            )
                            .await);
                        }
                        emitter
                            .emit(RuntimeEvent::Status {
                                phase: "tool_completed".to_string(),
                                message: if result.is_success() {
                                    "工具执行已完成。".to_string()
                                } else {
                                    "工具执行失败，正在整理可用信息。".to_string()
                                },
                                detail: Some(format!("工具：{}", tool_call.name)),
                                state: if result.is_success() {
                                    "completed".to_string()
                                } else {
                                    "error".to_string()
                                },
                            })
                            .await
                            .map_err(|_| "客户端连接已断开。".to_string())?;
                        report_transcript_failure(
                            append_task_state_record(
                                &self.state,
                                &turn_context,
                                if result.is_success() {
                                    "tool_completed"
                                } else {
                                    "tool_failed"
                                },
                                if result.is_success() {
                                    format!("工具 `{}` 已完成。", tool_call.name)
                                } else {
                                    format!("工具 `{}` 执行失败。", tool_call.name)
                                },
                                Some(&tool_call.name),
                            )
                            .await,
                        );

                        if tool_call.name == "persona_switch" && result.is_success() {
                            // persona_switch 会切换活动角色并创建新的空会话。旧角色回合的
                            // 私有副本不能覆盖该新会话；仅把切换结果发布到新会话中。
                            let mut switched_conversation =
                                self.state.runtime_service.conversation_snapshot().await;
                            switched_conversation.add_assistant_message(result.content.clone());
                            if let Err(err) = append_required_turn_event(
                                &self.state,
                                "assistant",
                                serde_json::json!({
                                    "conversation_id": turn_context.conversation_id,
                                    "turn_id": turn_context.turn_id,
                                    "content": result.content,
                                }),
                            )
                            .await
                            {
                                let message = format!("持久化角色切换回复失败：{err}");
                                let message = rollback_runtime_turn(
                                    &self.state,
                                    &emitter,
                                    message,
                                    Some(&turn_context),
                                )
                                .await;
                                return Err(message);
                            }
                            report_transcript_failure(
                                append_task_state_record(
                                    &self.state,
                                    &turn_context,
                                    "completed",
                                    "角色切换已完成，本轮已结束。",
                                    Some(&tool_call.name),
                                )
                                .await,
                            );
                            if let Err(err) = publish_committed_turn(
                                &self.state,
                                &emitter,
                                &turn_context,
                                switched_conversation,
                                "角色切换已完成，本轮状态已经提交。",
                                "角色已切换，本轮已结束。",
                            )
                            .await
                            {
                                let message = format!("提交角色切换回合失败：{err}");
                                let message = match err {
                                    PublishCommittedTurnError::CommitPersistence(_) => {
                                        rollback_runtime_turn(
                                            &self.state,
                                            &emitter,
                                            message,
                                            Some(&turn_context),
                                        )
                                        .await
                                    }
                                    PublishCommittedTurnError::MemoryPublish(_) => {
                                        emit_committed_memory_publish_failure(&emitter, &message)
                                            .await;
                                        message
                                    }
                                };
                                return Err(message);
                            }
                            return Ok(RuntimeTurnOutcome {
                                reply: result.content,
                            });
                        }

                        if tool_call.name == "session_compact" && result.is_success() {
                            apply_session_compaction_result(&mut working_conversation, &result);
                        }
                        working_conversation.add_tool_result_with_status(
                            tool_call.call_id.clone(),
                            tool_call.name.clone(),
                            tool_result_content_for_model(&tool_call.name, &result),
                            !result.is_success(),
                        );
                        produced_tool_output = true;
                        if result.is_success()
                            && matches!(
                                tool_call.name.as_str(),
                                "enter_plan_mode" | "exit_plan_mode"
                            )
                        {
                            let next_mode = runtime_mode_state_from_tool_result(&result)
                                .ok_or_else(|| {
                                    format!(
                                        "工具 `{}` 未返回有效的 runtime_mode_state，已拒绝改变本轮能力。",
                                        tool_call.name
                                    )
                                })?;
                            let visible_tools = visible_tool_definitions_for_turn(
                                turn_snapshot.full_tool_definitions(),
                                next_mode.tool_preset(),
                                &turn_snapshot.context.runtime_policy.skill_tool_ids,
                            );
                            let mut next_prompt = build_runtime_system_prompt_with_mode_state(
                                &self.state.config,
                                Some(&active_persona),
                                &visible_tools,
                                next_mode,
                            );
                            append_frozen_skill_catalog(
                                &mut next_prompt,
                                &turn_snapshot.context.runtime_policy.skill_catalog,
                                turn_snapshot.context.runtime_policy.omitted_skill_count,
                            );
                            if let Some(prompt) = activated_skill_prompt.as_deref() {
                                next_prompt.push_str(prompt);
                            }
                            turn_snapshot.transition_runtime_mode(
                                next_mode.mode.as_str(),
                                next_mode.focus_phase.as_str(),
                                next_mode.tool_preset().as_str(),
                                visible_tools,
                            )?;
                            turn_snapshot.context.system_prompt = next_prompt;
                            turn_context = turn_snapshot.context.clone();
                            // 当前模型响应是在旧能力 epoch 下生成的，剩余工具调用全部作废。
                            break;
                        }
                    }
                }
            }

            if !produced_tool_output {
                report_transcript_failure(
                    append_task_state_record(
                        &self.state,
                        &turn_context,
                        "completed",
                        "本轮回复已完成。",
                        None,
                    )
                    .await,
                );
                if let Err(err) = publish_committed_turn(
                    &self.state,
                    &emitter,
                    &turn_context,
                    working_conversation.clone(),
                    "模型回复与工具结果已经提交。",
                    "回复已完成。",
                )
                .await
                {
                    let message = format!("提交对话回合失败：{err}");
                    let message = match err {
                        PublishCommittedTurnError::CommitPersistence(_) => {
                            rollback_runtime_turn(
                                &self.state,
                                &emitter,
                                message,
                                Some(&turn_context),
                            )
                            .await
                        }
                        PublishCommittedTurnError::MemoryPublish(_) => {
                            emit_committed_memory_publish_failure(&emitter, &message).await;
                            message
                        }
                    };
                    return Err(message);
                }
                return Ok(RuntimeTurnOutcome { reply: final_reply });
            }

            update_conversation_system_prompt(
                &mut working_conversation,
                turn_snapshot.context.system_prompt.clone(),
            );
            let conversation_for_followup = working_conversation.clone();
            if let Err(err) = conversation_for_followup.validate_tool_protocol() {
                let message = format!("工具续轮协议校验失败：{err}");
                let message =
                    rollback_runtime_turn(&self.state, &emitter, message, Some(&turn_context))
                        .await;
                return Err(message);
            }
            emitter
                .emit(RuntimeEvent::Status {
                    phase: "synthesizing".to_string(),
                    message: "正在根据工具结果整理最终回复。".to_string(),
                    detail: Some(format!(
                        "当前工具预设：{}。",
                        turn_snapshot.context.tool_preset
                    )),
                    state: "active".to_string(),
                })
                .await
                .map_err(|_| "客户端连接已断开。".to_string())?;
            report_transcript_failure(
                append_task_state_record(
                    &self.state,
                    &turn_context,
                    "synthesizing",
                    "正在根据工具结果整理最终回复。",
                    None,
                )
                .await,
            );
            let context_snapshot = build_runtime_context_snapshot(
                &turn_context,
                &conversation_for_followup,
                frozen_context_profile,
            );
            if let Err(err) = turn_snapshot.consume_model_request() {
                return Err(
                    rollback_runtime_turn(&self.state, &emitter, err, Some(&turn_context)).await,
                );
            }
            let followup_stream_result = match turn_snapshot
                .run_with_deadline(stream_provider_reply(
                    &self.state,
                    &provider_snapshot,
                    &emitter,
                    &conversation_for_followup,
                    turn_snapshot.visible_tool_definitions(),
                    &cancel_token,
                ))
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    let message = err.to_string();
                    let message =
                        rollback_runtime_turn(&self.state, &emitter, message, Some(&turn_context))
                            .await;
                    return Err(message);
                }
            };
            current_turn = match followup_stream_result {
                Ok(reply) => reply,
                Err(err) => {
                    return Err(rollback_runtime_turn(
                        &self.state,
                        &emitter,
                        err.to_string(),
                        Some(&turn_context),
                    )
                    .await);
                }
            };
            current_model_epoch = turn_snapshot.capability_epoch();
            emit_and_record_runtime_usage(
                &self.state,
                &emitter,
                &turn_context,
                context_snapshot,
                &current_turn,
            )
            .await;
        }
    }
}

async fn durable_before_publish<Persist, PersistFuture, Publish, PublishFuture>(
    persist: Persist,
    publish: Publish,
) -> Result<(), String>
where
    Persist: FnOnce() -> PersistFuture,
    PersistFuture: Future<Output = Result<(), String>>,
    Publish: FnOnce() -> PublishFuture,
    PublishFuture: Future<Output = Result<(), String>>,
{
    persist().await?;
    publish().await
}

async fn durable_failure_before_publish<Persist, PersistFuture, Publish, PublishFuture>(
    persist: Persist,
    publish: Publish,
) -> Result<(), String>
where
    Persist: FnOnce() -> PersistFuture,
    PersistFuture: Future<Output = Result<(), String>>,
    Publish: FnOnce() -> PublishFuture,
    PublishFuture: Future<Output = Result<(), String>>,
{
    durable_before_publish(persist, publish).await
}

#[derive(Debug)]
enum PublishCommittedTurnError {
    CommitPersistence(String),
    MemoryPublish(String),
}

impl std::fmt::Display for PublishCommittedTurnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommitPersistence(message) | Self::MemoryPublish(message) => {
                formatter.write_str(message)
            }
        }
    }
}

async fn emit_committed_memory_publish_failure(emitter: &RuntimeEventEmitter, message: &str) {
    let _ = emitter
        .emit(RuntimeEvent::Error {
            message: format!(
                "{message}。`turn_committed` 已可靠写入，当前进程不会追加冲突的 aborted 终态；重启后将从 transcript 恢复。"
            ),
        })
        .await;
    let _ = emitter.emit(RuntimeEvent::Done).await;
}

async fn publish_committed_turn(
    state: &Arc<AppState>,
    emitter: &RuntimeEventEmitter,
    turn: &TurnContext,
    conversation: Conversation,
    summary: &str,
    status_message: &str,
) -> Result<(), PublishCommittedTurnError> {
    append_turn_committed(state, turn, summary)
        .await
        .map_err(PublishCommittedTurnError::CommitPersistence)?;
    state
        .runtime_service
        .publish_turn_conversation(&turn.turn_id, conversation)
        .await
        .map_err(|err| PublishCommittedTurnError::MemoryPublish(err.to_string()))?;
    let _ = emitter
        .emit(RuntimeEvent::Status {
            phase: "completed".to_string(),
            message: status_message.to_string(),
            detail: None,
            state: "completed".to_string(),
        })
        .await;
    let _ = emitter.emit(RuntimeEvent::Done).await;
    Ok(())
}

async fn append_turn_committed(
    state: &Arc<AppState>,
    turn: &TurnContext,
    summary: &str,
) -> Result<(), String> {
    append_required_turn_event(
        state,
        "turn_committed",
        serde_json::json!({
            "conversation_id": turn.conversation_id,
            "turn_id": turn.turn_id,
            "outcome": "committed",
            "summary": summary,
        }),
    )
    .await
}

fn report_transcript_failure(result: Result<(), String>) {
    if let Err(err) = result {
        tracing::error!(target: "muse::transcript", error = %err, "写入非终态会话事件失败");
    }
}

fn rollback_terminal_kind(had_external_effects: bool) -> &'static str {
    if had_external_effects {
        "turn_interrupted_with_effects"
    } else {
        "turn_aborted"
    }
}

async fn abort_preparing_turn(
    state: &Arc<AppState>,
    emitter: &RuntimeEventEmitter,
    conversation_id: &str,
    turn_id: &str,
    message: String,
) -> String {
    let terminal_payload = serde_json::json!({
        "conversation_id": conversation_id,
        "turn_id": turn_id,
        "outcome": "aborted",
        "message": message.clone(),
        "phase": "preparing",
    });
    let publish_message = message.clone();
    let result = durable_failure_before_publish(
        || append_required_turn_event(state, "turn_aborted", terminal_payload),
        || async {
            let _ = emitter
                .emit(RuntimeEvent::Error {
                    message: publish_message,
                })
                .await;
            let _ = emitter.emit(RuntimeEvent::Done).await;
            Ok(())
        },
    )
    .await;
    match result {
        Ok(()) => message,
        Err(error) => format!("{message}；持久化准备阶段终态失败：{error}"),
    }
}

async fn rollback_runtime_turn(
    state: &Arc<AppState>,
    emitter: &RuntimeEventEmitter,
    message: String,
    turn_context: Option<&TurnContext>,
) -> String {
    let had_external_effects =
        turn_context.is_some_and(|turn| active_turn_had_external_effects(state, &turn.turn_id));
    report_transcript_failure(
        append_transcript_record(
            state,
            "error",
            serde_json::json!({
                "conversation_id": active_conversation_id(state),
                "turn_id": turn_context.map(|turn| turn.turn_id.clone()),
                "content": message.clone(),
                "had_external_effects": had_external_effects,
            }),
        )
        .await,
    );
    if let Some(turn_context) = turn_context {
        let terminal_kind = rollback_terminal_kind(had_external_effects);
        let terminal_payload = serde_json::json!({
            "conversation_id": turn_context.conversation_id,
            "turn_id": turn_context.turn_id,
            "outcome": if had_external_effects { "interrupted_with_effects" } else { "aborted" },
            "message": message.clone(),
        });
        let recovery_payload = terminal_payload.clone();
        let publish_message = message.clone();
        let terminal_result = durable_failure_before_publish(
            || append_required_turn_event(state, terminal_kind, terminal_payload),
            || async {
                if had_external_effects {
                    // 真实外部副作用不能随私有副本一起回滚。终态可靠落盘后，
                    // 只向最后一个已发布会话附加结构化恢复提示。
                    let mut conversation = state.runtime_service.lock_conversation().await;
                    conversation.add_assistant_message(interrupted_turn_recovery_context(
                        &recovery_payload,
                    ));
                    drop(conversation);
                    state.runtime_service.touch();
                }
                report_transcript_failure(
                    append_task_state_record(
                        state,
                        turn_context,
                        "failed",
                        if had_external_effects {
                            "本轮中断前已发生外部副作用；会话已回滚并附加恢复上下文。"
                        } else {
                            "本轮没有外部副作用，内存会话已回滚到本轮开始前。"
                        },
                        None,
                    )
                    .await,
                );
                let _ = emitter
                    .emit(RuntimeEvent::Error {
                        message: publish_message,
                    })
                    .await;
                let _ = emitter.emit(RuntimeEvent::Done).await;
                Ok(())
            },
        )
        .await;
        if let Err(err) = terminal_result {
            let persistence_failure = format!(
                "{message}\n关键回合终态 `{terminal_kind}` 持久化失败：{err}。运行时已停止宣告本轮完成；重启后将按未提交回合处理。"
            );
            if had_external_effects {
                let mut conv = state.runtime_service.lock_conversation().await;
                conv.add_assistant_message(format!(
                    "[结构化持久化故障]\n{}",
                    serde_json::json!({
                        "kind": "turn_terminal_persistence_failed",
                        "turn_id": turn_context.turn_id,
                        "terminal_kind": terminal_kind,
                        "message": err,
                        "instruction": "不得把本轮视为可靠完成；继续前先检查 transcript 与外部状态。"
                    })
                ));
                conv.add_assistant_message(interrupted_turn_recovery_context(&recovery_payload));
                drop(conv);
                state.runtime_service.touch();
            }
            let _ = emitter
                .emit(RuntimeEvent::Error {
                    message: persistence_failure.clone(),
                })
                .await;
            return persistence_failure;
        }
        return message;
    }
    let _ = emitter
        .emit(RuntimeEvent::Error {
            message: message.clone(),
        })
        .await;
    let _ = emitter.emit(RuntimeEvent::Done).await;
    message
}

include!("api/runtime_assets_voice.rs");
include!("api/app_preferences.rs");
include!("api/chat_sessions_models.rs");
include!("api/runtime_policy.rs");
include!("api/personas_stream.rs");
include!("api/persona_session_binding.rs");
include!("api/skills_management.rs");
include!("api/mcp_management.rs");
include!("tools/registry.rs");
include!("tools/interaction.rs");
include!("tools/files.rs");
include!("tools/command.rs");
include!("tools/network.rs");
include!("tools/mcp.rs");
include!("tools/session.rs");
include!("tools/persona_context.rs");
include!("tests.rs");
