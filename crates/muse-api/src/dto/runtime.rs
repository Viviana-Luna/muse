//! 运行时控制接口的请求与响应 DTO。

use muse_core::domain::runtime::RuntimeTodoItem;
use muse_core::domain::usage::{RuntimeContextSnapshot, RuntimeTokenUsage};
use serde::{Deserialize, Serialize};

/// 运行时交互决策公共请求体。
#[derive(Deserialize)]
pub struct RuntimeDecisionRequest {
    pub turn_id: String,
}

/// 工具审批拒绝请求体。
#[derive(Deserialize)]
pub struct ApprovalRejectRequest {
    pub turn_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// 工具审批决策响应体。
#[derive(Serialize)]
pub struct ApprovalDecisionResponse {
    pub approval_id: String,
    pub approved: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 用户问题回答请求体。
#[derive(Deserialize)]
pub struct UserQuestionAnswerRequest {
    pub turn_id: String,
    pub answers: serde_json::Value,
    #[serde(default)]
    pub annotations: Option<serde_json::Value>,
}

/// 用户问题决策响应体。
#[derive(Serialize)]
pub struct UserQuestionDecisionResponse {
    pub request_id: String,
    pub answered: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 运行底座工作区策略更新请求体。
#[derive(Deserialize)]
pub struct RuntimeWorkspacePolicyRequest {
    pub permission_mode: String,
    pub sandbox_mode: String,
}

/// 运行底座可见工作区根目录信息。
#[derive(Serialize)]
pub struct RuntimeWorkspaceRootInfo {
    pub path: String,
    pub label: String,
    pub kind: String,
    pub exists: bool,
    pub writable: bool,
    pub removable: bool,
}

/// 运行底座工作区策略与根目录响应体。
#[derive(Serialize)]
pub struct RuntimeWorkspacesResponse {
    pub roots: Vec<RuntimeWorkspaceRootInfo>,
    pub permission_mode: String,
    pub sandbox_mode: String,
}

/// 当前会话审批模式更新请求。
#[derive(Deserialize)]
pub struct RuntimeApprovalModeUpdateRequest {
    pub preset: String,
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

/// 当前会话审批模式的规范化三轴事实。
#[derive(Serialize)]
pub struct RuntimeApprovalModeResponse {
    pub conversation_id: String,
    pub preset: String,
    pub approval_policy: String,
    pub approvals_reviewer: String,
    pub permission_profile: String,
    pub revision: u64,
    pub status: String,
}

/// 运行模式切换请求体。
#[derive(Deserialize)]
pub struct RuntimeModeUpdateRequest {
    pub mode: String,
    #[serde(default)]
    pub focus_phase: Option<String>,
}

/// 当前运行模式响应体。
#[derive(Serialize)]
pub struct RuntimeModeResponse {
    pub mode: String,
    pub focus_phase: String,
    pub tool_preset: String,
    pub status: String,
}

/// 活动回合摘要。
#[derive(Serialize)]
pub struct RuntimeBusyTurnResponse {
    pub turn_id: String,
    pub phase: String,
}

/// 运行时事实状态。`state_revision` 在进程生命周期内严格单调递增。
#[derive(Serialize)]
pub struct RuntimeStateResponse {
    pub state_revision: u64,
    pub active_persona_id: Option<String>,
    pub active_conversation_id: String,
    pub mode: String,
    pub focus_phase: String,
    pub busy_turn: Option<RuntimeBusyTurnResponse>,
    pub exclusive_operation: Option<String>,
    pub usage_summary: serde_json::Value,
    pub context_summary: serde_json::Value,
}

/// 当前运行时任务清单响应体。
#[derive(Serialize)]
pub struct RuntimeTodosResponse {
    pub todos: Vec<RuntimeTodoItem>,
    pub status: String,
}

/// 运行时 Token 用量查询参数。
#[derive(Deserialize)]
pub struct RuntimeTokenUsageQuery {
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub range: Option<String>,
}

/// 运行时上下文快照查询参数。
#[derive(Deserialize)]
pub struct RuntimeContextSnapshotQuery {
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// Token 用量聚合字段。
#[derive(Debug, Clone, Default, Serialize)]
pub struct RuntimeTokenUsageBreakdown {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub server_tool_tokens: u64,
    pub total_tokens: u64,
}

/// 按来源聚合的 Token 用量。
#[derive(Serialize)]
pub struct RuntimeTokenUsageSourceSummary {
    pub source: String,
    pub records: usize,
    pub total_tokens: u64,
}

/// 按模型聚合的 Token 用量。
#[derive(Serialize)]
pub struct RuntimeTokenUsageModelSummary {
    pub provider: String,
    pub model: String,
    pub records: usize,
    pub total_tokens: u64,
}

/// 运行时 Token 用量响应。
#[derive(Serialize)]
pub struct RuntimeTokenUsageResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub range: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub to: String,
    pub records: usize,
    pub totals: RuntimeTokenUsageBreakdown,
    pub by_source: Vec<RuntimeTokenUsageSourceSummary>,
    pub by_model: Vec<RuntimeTokenUsageModelSummary>,
    pub items: Vec<RuntimeTokenUsage>,
    pub status: String,
}

/// 最新上下文快照响应。
#[derive(Serialize)]
pub struct RuntimeContextSnapshotResponse {
    pub conversation_id: String,
    pub snapshot: Option<RuntimeContextSnapshot>,
    pub status: String,
}
