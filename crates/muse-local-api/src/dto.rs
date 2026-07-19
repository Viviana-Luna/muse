//! 网页接口 DTO 模块，集中定义请求体、响应体和运行时配置传输结构。

use muse_core::app::preferences::WebSearchProvider;
use muse_core::domain::conversation::Message;
use muse_core::domain::mcp;
use muse_core::domain::persona::character::card::{
    PersonaCard, PersonaCardConflictStrategy, PersonaCardExportLevel,
};
use muse_core::domain::persona::visual::VisualPack;
use muse_core::domain::persona::{Persona, PersonaSummary};
use muse_core::domain::runtime::RuntimeTodoItem;
use muse_core::domain::usage::{RuntimeContextSnapshot, RuntimeTokenUsage};
use muse_core::model::catalog::{ModelCatalogItem, ModelCatalogModelDraft};
use muse_core::model::vendor::ProviderBalanceInfo;
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

static API_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_api_request_id() -> String {
    format!(
        "req-{}-{}",
        std::process::id(),
        API_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

/// 普通聊天请求体。
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default)]
    pub selected_skill: Option<String>,
}

/// 流式聊天请求体。
#[derive(Deserialize)]
pub struct ChatStreamRequest {
    pub message: String,
    pub conversation_id: String,
    pub client_request_id: String,
    #[serde(default)]
    pub voice_enabled: Option<bool>,
    #[serde(default)]
    pub selected_skill: Option<String>,
}

/// 当前角色在新回合中可选择的有效 Skill 目录。
#[derive(Serialize)]
pub struct RuntimeSkillCatalogResponse {
    pub skills: Vec<muse_core::domain::turn::RuntimeSkillCatalogEntry>,
    pub omitted_skill_count: usize,
}

/// WebSocket 握手短票据查询参数。
#[derive(Deserialize)]
pub struct WsTicketQuery {
    #[serde(default)]
    pub ticket: Option<String>,
}

/// 普通聊天响应体。
#[derive(Serialize)]
pub struct ChatResponse {
    pub reply: String,
}

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

/// 单项连通性诊断结果。
#[derive(Serialize)]
pub struct DiagnosticsConnectivityItem {
    pub id: String,
    pub label: String,
    pub target: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub message: String,
}

/// 连通性诊断响应体。
#[derive(Serialize)]
pub struct DiagnosticsConnectivityResponse {
    pub checks: Vec<DiagnosticsConnectivityItem>,
}

/// 当前会话历史响应体。
#[derive(Serialize)]
pub struct HistoryResponse {
    pub messages: Vec<Message>,
}

/// 恢复运行时会话后的响应体。
#[derive(Serialize)]
pub struct RuntimeSessionResumeResponse {
    pub conversation_id: String,
    pub persona_id: String,
    pub persona_name_snapshot: String,
    pub persona_version_snapshot: String,
    pub persona_status: String,
    pub restored_messages: usize,
    pub status: String,
}

/// 运行时会话列表响应体。
#[derive(Serialize)]
pub struct RuntimeSessionListResponse {
    pub sessions: Vec<serde_json::Value>,
    pub active_conversation_id: String,
    pub status: String,
}

/// 修改会话标题或归档状态。
#[derive(Deserialize)]
pub struct RuntimeSessionMetadataPatch {
    /// `None` 表示未提交；JSON `null` 表示清除自定义标题。
    #[serde(default)]
    pub title: Option<Option<String>>,
    #[serde(default)]
    pub archived: Option<bool>,
}

#[derive(Serialize)]
pub struct RuntimeSessionMetadataResponse {
    pub conversation_id: String,
    pub persona_id: String,
    pub persona_name_snapshot: String,
    pub persona_version_snapshot: String,
    pub persona_status: String,
    pub title: Option<String>,
    pub archived: bool,
    pub source_conversation_id: Option<String>,
    pub updated_at: String,
    pub revision: u64,
}

#[derive(Serialize)]
pub struct RuntimeSessionExportMessage {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct RuntimeSessionExportResponse {
    pub schema_version: String,
    pub conversation_id: String,
    pub persona_id: String,
    pub persona_name_snapshot: String,
    pub persona_version_snapshot: String,
    pub persona_status: String,
    pub title: Option<String>,
    pub archived: bool,
    pub source_conversation_id: Option<String>,
    pub exported_at: String,
    pub messages: Vec<RuntimeSessionExportMessage>,
}

#[derive(Serialize)]
pub struct RuntimeSessionContextResponse {
    pub conversation_id: String,
    pub context_snapshot: Option<RuntimeContextSnapshot>,
    pub runtime_policy_snapshot: Option<serde_json::Value>,
    pub status: String,
}

/// 运行时会话分叉请求体。
#[derive(Deserialize)]
pub struct RuntimeSessionForkRequest {
    #[serde(default)]
    pub before_user_message_index: Option<usize>,
    #[serde(default)]
    pub target_persona_id: Option<String>,
}

/// 运行时会话分叉响应体。
#[derive(Serialize)]
pub struct RuntimeSessionForkResponse {
    pub conversation_id: String,
    pub persona_id: String,
    pub persona_name_snapshot: String,
    pub persona_version_snapshot: String,
    pub persona_status: String,
    pub source_conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_user_message_index: Option<usize>,
    pub restored_messages: usize,
    pub status: String,
}

/// 删除运行时会话后的响应体。
#[derive(Serialize)]
pub struct RuntimeSessionDeleteResponse {
    pub conversation_id: String,
    pub active_conversation_id: String,
    pub deleted_records: usize,
    pub deleted_files: usize,
    pub status: String,
}

/// 当前模型概要响应体。
#[derive(Serialize)]
pub struct ModelsResponse {
    /// 兼容旧前端的供应商展示名。
    pub provider: String,
    /// 兼容旧前端的上游模型 ID。
    pub model: String,
    pub provider_id: String,
    pub provider_name: String,
    pub model_name: String,
}

/// 角色创建或更新请求体。
#[derive(Deserialize)]
pub struct PersonaUpsertRequest {
    pub persona: Persona,
    #[serde(default)]
    pub visual_pack_patch: Option<PersonaVisualPackPatch>,
    /// 创建成功后在同一 transition gate 内激活角色并重置会话。
    #[serde(default)]
    pub activate_after_create: bool,
}

/// 角色编辑页提交的展示包补丁。
#[derive(Clone, Deserialize)]
pub struct PersonaVisualPackPatch {
    pub portrait_path: String,
    #[serde(default)]
    pub background_path: Option<String>,
    #[serde(default)]
    pub avatar_path: Option<String>,
    #[serde(default)]
    pub theme_color: Option<String>,
    #[serde(default)]
    pub theme_mode: Option<String>,
    #[serde(default)]
    pub portrait_frame: Option<String>,
    #[serde(default)]
    pub portrait_fit: Option<String>,
    #[serde(default)]
    pub portrait_position_x: Option<i32>,
    #[serde(default)]
    pub portrait_position_y: Option<i32>,
    #[serde(default)]
    pub portrait_scale: Option<u16>,
}

/// 图片上传响应体。
#[derive(Serialize)]
pub struct AssetUploadResponse {
    pub url: String,
}

/// 角色卡片导出查询参数。
#[derive(Deserialize)]
pub struct PersonaCardExportQuery {
    #[serde(default)]
    pub level: PersonaCardExportLevel,
}

/// 角色卡片导入请求体。
#[derive(Deserialize)]
pub struct PersonaCardImportRequest {
    pub card: PersonaCard,
    #[serde(default)]
    pub conflict_strategy: PersonaCardConflictStrategy,
    #[serde(default)]
    pub activate_after_import: bool,
}

/// 角色列表响应体。
#[derive(Serialize)]
pub struct PersonaListResponse {
    pub personas: Vec<PersonaLibraryItem>,
    pub active_persona_id: Option<String>,
}

/// 角色库列表项，保留轻量摘要并附带已解析的图片预览事实。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersonaLibraryItem {
    #[serde(flatten)]
    pub persona: PersonaSummary,
    pub visual_preview: PersonaVisualPreview,
}

/// 角色库卡片可使用的图片路径；缺失保持为 `null`，不伪造首字头像。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersonaVisualPreview {
    pub avatar_path: Option<String>,
    pub portrait_path: Option<String>,
}

impl PersonaLibraryItem {
    pub fn from_summary(persona: PersonaSummary, visual_pack: Option<&VisualPack>) -> Self {
        let visual_preview = PersonaVisualPreview {
            avatar_path: visual_pack.and_then(|pack| normalized_visual_path(&pack.avatar_path)),
            portrait_path: visual_pack.and_then(|pack| normalized_visual_path(&pack.portrait_path)),
        };
        Self {
            persona,
            visual_preview,
        }
    }
}

fn normalized_visual_path(path: &str) -> Option<String> {
    let normalized = path.trim();
    (!normalized.is_empty()).then(|| normalized.to_string())
}

/// 当前激活角色响应体。
#[derive(Serialize)]
pub struct ActivePersonaResponse {
    pub active_persona: Option<Persona>,
    pub active_persona_id: Option<String>,
    pub visual_pack: Option<VisualPack>,
    pub state_revision: u64,
}

/// 角色详情响应体。
#[derive(Serialize)]
pub struct PersonaDetailResponse {
    pub persona: Persona,
    pub visual_pack: Option<VisualPack>,
}

#[derive(Serialize)]
pub struct PersonaDeletionImpactResponse {
    pub persona_id: String,
    pub associated_session_count: usize,
    pub workspace_state_exists: bool,
}

/// 角色变更响应体。
#[derive(Serialize)]
pub struct PersonaMutationResponse {
    pub affected_persona: Persona,
    pub active_persona: Option<Persona>,
    pub active_persona_id: Option<String>,
    pub visual_pack: Option<VisualPack>,
    pub runtime_reset: bool,
    pub conversation_id: String,
    pub active_conversation_id: String,
    pub session_restored: bool,
    pub state_revision: u64,
}

/// 角色卡片导入响应体。
#[derive(Serialize)]
pub struct PersonaCardImportResponse {
    pub affected_persona: Persona,
    pub active_persona: Option<Persona>,
    pub active_persona_id: Option<String>,
    pub visual_pack: Option<VisualPack>,
    pub notices: Vec<String>,
    pub runtime_reset: bool,
    pub conversation_id: String,
    pub state_revision: u64,
}

/// 通用状态响应体。
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: String,
}

/// Skill 创建请求。
#[derive(Deserialize)]
pub struct SkillCreateRequest {
    pub name: String,
    pub description: String,
    pub content: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Skill 更新请求。
#[derive(Deserialize)]
pub struct SkillUpdateRequest {
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub revision: String,
}

/// 使用 revision 的删除请求。
#[derive(Deserialize)]
pub struct RevisionQuery {
    pub revision: String,
}

fn default_true() -> bool {
    true
}

/// MCP 凭据字段的变更请求。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSecretFieldUpdate {
    pub target: String,
    #[serde(default)]
    pub secret: SecretUpdate,
}

/// MCP stdio 或 Streamable HTTP 传输配置。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportUpdate {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        secrets: Vec<McpSecretFieldUpdate>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        header_secrets: Vec<McpSecretFieldUpdate>,
        #[serde(default)]
        bearer_token: Option<McpSecretFieldUpdate>,
    },
}

/// MCP server 创建请求。
#[derive(Debug, Deserialize)]
pub struct McpServerCreateRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub approval_policy: mcp::McpApprovalPolicy,
    #[serde(default)]
    pub tool_approval_overrides: BTreeMap<String, mcp::McpApprovalPolicy>,
    pub transport: McpTransportUpdate,
}

/// 不落盘的严格单 Server 草稿测试请求。
#[derive(Debug, Deserialize)]
pub struct McpDraftTestRequest {
    #[serde(default)]
    pub source_name: Option<String>,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub include_resources: bool,
    pub server: McpServerCreateRequest,
}

/// MCP server 更新请求。
#[derive(Debug, Deserialize)]
pub struct McpServerUpdateRequest {
    pub revision: String,
    #[serde(flatten)]
    pub server: McpServerCreateRequest,
}

/// MCP 凭据的非敏感状态。
#[derive(Debug, Serialize)]
pub struct McpSecretFieldState {
    pub target: String,
    pub configured: bool,
}

/// MCP 传输配置响应。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportResponse {
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: BTreeMap<String, String>,
        secrets: Vec<McpSecretFieldState>,
    },
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, String>,
        header_secrets: Vec<McpSecretFieldState>,
        bearer_token: Option<McpSecretFieldState>,
    },
}

/// MCP server 列表项。
#[derive(Debug, Serialize)]
pub struct McpServerSummaryResponse {
    pub name: String,
    pub enabled: bool,
    pub transport: String,
    pub revision: String,
    pub status: String,
    pub tested_revision: Option<String>,
    pub tool_count: usize,
    pub resource_count: usize,
    pub last_checked_at: Option<String>,
    pub last_error: Option<mcp::McpStructuredError>,
    pub policy_reason: Option<String>,
}

/// MCP server 详情。
#[derive(Debug, Serialize)]
pub struct McpServerDetailResponse {
    pub name: String,
    pub enabled: bool,
    pub request_timeout_ms: Option<u64>,
    pub enabled_tools: Option<Vec<String>>,
    pub disabled_tools: Vec<String>,
    pub approval_policy: mcp::McpApprovalPolicy,
    pub tool_approval_overrides: BTreeMap<String, mcp::McpApprovalPolicy>,
    pub transport: McpTransportResponse,
    pub revision: String,
}

/// MCP 连接测试或目录刷新结果。
#[derive(Debug, Serialize)]
pub struct McpCatalogResponse {
    pub server: String,
    pub revision: String,
    pub status: String,
    pub tools: Vec<Value>,
    pub resources: Vec<Value>,
    pub errors: Vec<Value>,
    pub refreshed_at: String,
    pub diagnostic: McpCatalogDiagnosticResponse,
}

/// MCP 测试返回的本地诊断与固定网络策略。
#[derive(Debug, Serialize)]
pub struct McpCatalogDiagnosticResponse {
    pub connection: Option<mcp::McpConnectionDiagnostic>,
    pub policy_reason: Option<String>,
    pub redirect_policy: &'static str,
    pub proxy_policy: &'static str,
    pub sensitive_headers: &'static str,
}

/// 通用错误响应体。
#[derive(Debug)]
pub struct ErrorResponse {
    pub error: String,
}

impl Serialize for ErrorResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let code = infer_api_error_code(&self.error);
        let message = public_api_error_message(&self.error);
        let retryable = self.error.contains("超时")
            || self.error.contains("暂时")
            || self.error.contains("连接失败")
            || self.error.starts_with("provider_rate_limited")
            || self.error.starts_with("provider_timeout")
            || self.error.starts_with("provider_unreachable");
        let busy = busy_error_details(&self.error);
        let mut state =
            serializer.serialize_struct("ApiError", if busy.is_some() { 8 } else { 6 })?;
        // 兼容首个协议过渡期的旧客户端；新客户端统一读取 message/code。
        state.serialize_field("error", message)?;
        state.serialize_field("code", code)?;
        state.serialize_field("message", message)?;
        state.serialize_field("field_errors", &BTreeMap::<String, String>::new())?;
        state.serialize_field("retryable", &retryable)?;
        state.serialize_field("request_id", &next_api_request_id())?;
        if let Some((turn_id, phase)) = busy {
            state.serialize_field("turn_id", &turn_id)?;
            state.serialize_field("phase", phase)?;
        }
        state.end()
    }
}

fn busy_error_details(message: &str) -> Option<(String, &'static str)> {
    if !message.starts_with("runtime_busy") {
        return None;
    }
    let marker = "当前回合 `";
    let turn_start = message.find(marker)?.saturating_add(marker.len());
    let turn_end = message[turn_start..].find('`')?.saturating_add(turn_start);
    let phase = [
        ("Preparing", "preparing"),
        ("Running", "running"),
        ("WaitingApproval", "waiting_approval"),
        ("WaitingUser", "waiting_user"),
        ("Cancelling", "cancelling"),
        ("Finalizing", "finalizing"),
    ]
    .into_iter()
    .find_map(|(raw, normalized)| message.contains(raw).then_some(normalized))?;
    Some((message[turn_start..turn_end].to_string(), phase))
}

#[cfg(test)]
mod api_error_tests {
    use super::ErrorResponse;

    #[test]
    fn runtime_busy_error_includes_turn_and_phase() {
        let value = serde_json::to_value(ErrorResponse {
            error: "runtime_busy：当前回合 `turn-42` 仍处于 WaitingApproval 阶段。".to_string(),
        })
        .expect("busy 错误应能序列化");

        assert_eq!(value["code"], "runtime_busy");
        assert_eq!(value["turn_id"], "turn-42");
        assert_eq!(value["phase"], "waiting_approval");
    }

    #[test]
    fn persona_required_error_exposes_stable_code_without_internal_marker() {
        let value = serde_json::to_value(ErrorResponse {
            error: "persona_required：当前没有激活角色，请先选择角色。".to_string(),
        })
        .expect("角色门禁错误应能序列化");

        assert_eq!(value["code"], "persona_required");
        assert_eq!(value["message"], "当前没有激活角色，请先选择角色。");
        assert_eq!(value["error"], value["message"]);
    }

    #[test]
    fn provider_key_error_exposes_stable_code_without_internal_marker() {
        let value = serde_json::to_value(ErrorResponse {
            error: "provider_api_key_required：请先配置 API Key。".to_string(),
        })
        .expect("Provider 密钥错误应能序列化");

        assert_eq!(value["code"], "provider_api_key_required");
        assert_eq!(value["message"], "请先配置 API Key。");
    }

    #[test]
    fn provider_failures_expose_stable_codes_and_retryability() {
        for (raw, code, retryable) in [
            (
                "provider_auth_failed：凭据无效。",
                "provider_auth_failed",
                false,
            ),
            (
                "provider_rate_limited：请求过多。",
                "provider_rate_limited",
                true,
            ),
            (
                "provider_not_found：模型不存在。",
                "provider_not_found",
                false,
            ),
            (
                "provider_invalid_request：模型或参数无效。",
                "provider_invalid_request",
                false,
            ),
            ("provider_timeout：请求超时。", "provider_timeout", true),
            (
                "provider_protocol_error：响应格式错误。",
                "provider_protocol_error",
                false,
            ),
        ] {
            let value = serde_json::to_value(ErrorResponse {
                error: raw.to_string(),
            })
            .expect("Provider 错误应能序列化");
            assert_eq!(value["code"], code);
            assert_eq!(value["retryable"], retryable);
        }
    }
}

fn public_api_error_message(message: &str) -> &str {
    for prefix in [
        "persona_required：",
        "provider_api_key_required：",
        "provider_auth_failed：",
        "provider_rate_limited：",
        "provider_not_found：",
        "provider_invalid_request：",
        "provider_timeout：",
        "provider_protocol_error：",
        "provider_unreachable：",
        "skill_conflict：",
        "skill_revision_conflict：",
        "mcp_conflict：",
        "mcp_revision_conflict：",
        "mcp_config_invalid：",
        "mcp_revision_required：",
        "mcp_source_required：",
    ] {
        if let Some(public) = message.strip_prefix(prefix) {
            return public;
        }
    }
    message
}

fn infer_api_error_code(message: &str) -> &'static str {
    if message.starts_with("persona_required") {
        "persona_required"
    } else if message.starts_with("provider_api_key_required") {
        "provider_api_key_required"
    } else if message.starts_with("provider_auth_failed") {
        "provider_auth_failed"
    } else if message.starts_with("provider_rate_limited") {
        "provider_rate_limited"
    } else if message.starts_with("provider_not_found") {
        "provider_not_found"
    } else if message.starts_with("provider_invalid_request") {
        "provider_invalid_request"
    } else if message.starts_with("provider_timeout") {
        "provider_timeout"
    } else if message.starts_with("provider_protocol_error") {
        "provider_protocol_error"
    } else if message.starts_with("provider_unreachable") {
        "provider_unreachable"
    } else if message.starts_with("runtime_snapshot_unstable") {
        "runtime_snapshot_unstable"
    } else if message.starts_with("runtime_busy") {
        "runtime_busy"
    } else if message.starts_with("skill_conflict") {
        "skill_conflict"
    } else if message.starts_with("skill_revision_conflict") {
        "skill_revision_conflict"
    } else if message.starts_with("mcp_conflict") {
        "mcp_conflict"
    } else if message.starts_with("mcp_revision_conflict") {
        "mcp_revision_conflict"
    } else if message.starts_with("mcp_config_invalid") {
        "mcp_config_invalid"
    } else if message.starts_with("mcp_revision_required") {
        "mcp_revision_required"
    } else if message.starts_with("mcp_source_required") {
        "mcp_source_required"
    } else if message.contains("流式聊天只接受 POST") {
        "stream_post_required"
    } else if message.contains("已以不同结果处理") {
        "decision_conflict"
    } else if message.contains("已受理") {
        "duplicate_request"
    } else if message.contains("conversation_id 已过期") {
        "stale_conversation"
    } else if message.contains("不再等待")
        || message.contains("过期决策")
        || message.contains("属于其他回合")
    {
        "stale_turn"
    } else if message.contains("不存在") || message.contains("未找到") {
        "not_found"
    } else if message.contains("不能为空") || message.contains("必须") || message.contains("不支持")
    {
        "validation_error"
    } else {
        "request_failed"
    }
}

/// 文本转语音请求体。
#[derive(Deserialize)]
pub struct TtsRequest {
    pub text: String,
    #[serde(default)]
    pub voice_id: Option<String>,
}

/// 语音转文本响应体。
#[derive(Serialize)]
pub struct SpeechTranscriptionResponse {
    pub text: String,
}

/// 调试环境下扩展子服务探测响应体。
#[cfg(debug_assertions)]
#[derive(Serialize)]
pub struct DebugExpandServiceResponse {
    pub service: String,
    pub model_ready: bool,
    pub text: String,
}

/// 语音能力探测响应体。
#[derive(Serialize)]
pub struct VoiceCapabilitiesResponse {
    /// 语音合成是否可用（提供器已启用）。
    pub tts: bool,
    /// 语音识别服务是否可用。
    pub speech_recognition: bool,
}

/// 拉取提供器模型列表请求体。
#[derive(Deserialize)]
pub struct FetchModelCatalogRequest {
    pub provider: String,
    pub purpose: String,
    pub api_base: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// 拉取提供器模型列表响应体。
#[derive(Serialize)]
pub struct FetchModelCatalogResponse {
    pub provider_id: String,
    pub models: Vec<ModelCatalogItem>,
}

/// 模型目录创建与编辑请求。模型 ID 在编辑时保持不变。
pub type ModelCatalogMutationRequest = ModelCatalogModelDraft;

/// 删除模型目录项请求。
#[derive(Deserialize)]
pub struct ModelCatalogDeleteRequest {
    pub provider_id: String,
    pub model: String,
}

/// 切换当前聊天活动模型请求。
#[derive(Deserialize)]
pub struct ActiveChatModelUpdateRequest {
    pub provider_id: String,
    pub model: String,
}

/// 当前聊天活动模型响应，供应商与模型名称分别返回。
#[derive(Debug, Serialize)]
pub struct ActiveChatModelResponse {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub model_name: String,
}

/// 供应商凭据配置状态；响应不包含密钥、掩码或凭据引用。
#[derive(Serialize)]
pub struct ProviderCredentialResponse {
    pub provider_id: String,
    pub api_key_configured: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_diagnostic: Option<muse_core::model::catalog::ModelCredentialDiagnostic>,
}

/// 供应商启停请求。缺少字段会由 JSON 反序列化拒绝，避免把不完整请求误当作开启。
#[derive(Deserialize)]
pub struct ProviderStateUpdateRequest {
    pub enabled: bool,
}

/// 供应商专属余额检测请求体。
#[derive(Deserialize)]
pub struct ProviderBalanceRequest {
    pub provider: String,
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
}

/// 供应商专属余额检测响应体。
#[derive(Serialize)]
pub struct ProviderBalanceResponse {
    pub provider_id: String,
    pub is_available: bool,
    pub balance_infos: Vec<ProviderBalanceInfo>,
    pub status: String,
}

/// `GET /api/web-search/config` 响应：联网搜索后端与凭据配置状态。
/// 仅返回是否已配置，绝不返回 Exa API Key 或其掩码。
#[derive(Serialize)]
pub struct WebSearchConfigResponse {
    pub provider: WebSearchProvider,
    pub api_key_configured: bool,
}

/// 密钥变更动作。只有 `replace` 允许同时提交 `value`。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SecretUpdateAction {
    /// 保留当前存储中的值。
    #[default]
    Keep,
    /// 使用请求中的 `value` 替换当前值。
    Replace,
    /// 删除当前存储中的值。
    Delete,
}

/// 统一密钥更新契约，响应体永不回显 `value`。
#[derive(Debug, Default, Deserialize)]
pub struct SecretUpdate {
    #[serde(default)]
    pub action: SecretUpdateAction,
    #[serde(default)]
    pub value: Option<String>,
}

/// `PUT /api/web-search/config` 请求体：非敏感后端选择与密钥动作一次提交。
#[derive(Debug, Default, Deserialize)]
pub struct WebSearchConfigUpdate {
    #[serde(default)]
    pub provider: WebSearchProvider,
    #[serde(default)]
    pub action: SecretUpdateAction,
    #[serde(default)]
    pub value: Option<String>,
}

#[cfg(test)]
mod web_search_config_tests {
    use super::{SecretUpdateAction, WebSearchConfigResponse, WebSearchConfigUpdate};
    use muse_core::app::preferences::WebSearchProvider;

    #[test]
    fn web_search_config_response_never_contains_secret_value() {
        let response = WebSearchConfigResponse {
            provider: WebSearchProvider::ExaApi,
            api_key_configured: true,
        };
        let json = serde_json::to_value(response).expect("配置状态应能序列化");

        assert_eq!(json["provider"], "exa_api");
        assert_eq!(json["api_key_configured"], true);
        assert!(json.get("api_key").is_none());
    }

    #[test]
    fn web_search_config_update_accepts_explicit_replace() {
        let request: WebSearchConfigUpdate = serde_json::from_str(
            r#"{"provider":"exa_api","action":"replace","value":"exa-secret-value"}"#,
        )
        .expect("请求体应能解析");

        assert_eq!(request.provider, WebSearchProvider::ExaApi);
        assert_eq!(request.action, SecretUpdateAction::Replace);
        assert_eq!(request.value.as_deref(), Some("exa-secret-value"));
    }

    #[test]
    fn web_search_config_update_defaults_to_keep() {
        let request: WebSearchConfigUpdate = serde_json::from_str(r#"{}"#).expect("请求体应能解析");

        assert_eq!(request.provider, WebSearchProvider::ExaFreeMcp);
        assert_eq!(request.action, SecretUpdateAction::Keep);
        assert!(request.value.is_none());
    }
}

/// `GET /api/models/config` 响应：只暴露密钥是否存在，不返回明文或掩码。
#[derive(Serialize)]
pub struct ModelsConfigResponse {
    pub chat: ConfigSectionResponse,
    pub tts: TtsConfigResponse,
    pub speech_recognition: SpeechRecognitionConfigResponse,
    pub audio_understanding: ConfigSectionResponse,
    pub voice_input: VoiceInputConfigResponse,
}

/// 单段模型配置响应体。
#[derive(Serialize)]
pub struct ConfigSectionResponse {
    pub provider: String,
    pub api_base: String,
    pub api_protocol: String,
    pub api_key_configured: bool,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub voice_id: Option<String>,
    pub speed: Option<f32>,
}

/// 语音合成配置响应体。
#[derive(Serialize)]
pub struct TtsConfigResponse {
    pub enabled: bool,
    pub provider: String,
    pub api_base: String,
    pub api_key_configured: bool,
    pub model: String,
    pub voice_id: String,
    pub speed: f32,
    pub response_format: String,
}

#[cfg(test)]
mod model_secret_response_tests {
    use super::ConfigSectionResponse;

    #[test]
    fn model_config_response_only_exposes_configured_state() {
        let response = ConfigSectionResponse {
            provider: "openai".to_string(),
            api_base: "https://api.example.com/v1".to_string(),
            api_protocol: "chat_completions".to_string(),
            api_key_configured: true,
            model: "example".to_string(),
            max_tokens: Some(1024),
            temperature: Some(0.2),
            voice_id: None,
            speed: None,
        };
        let value = serde_json::to_value(response).expect("模型配置响应应能序列化");

        assert_eq!(value["api_key_configured"], true);
        assert!(value.get("api_key").is_none());
    }
}

/// 语音输入策略响应体。
#[derive(Serialize)]
pub struct VoiceInputConfigResponse {
    pub mode: String,
}

/// OpenAI-compatible 语音识别配置响应体。
#[derive(Serialize)]
pub struct SpeechRecognitionConfigResponse {
    pub enabled: bool,
    pub provider: String,
    pub api_base: String,
    pub api_key_configured: bool,
    pub model: String,
    pub language: String,
    pub response_format: String,
}

/// `PUT /api/models/config` 请求体；`api_key` 为空表示保持原值。
#[derive(Deserialize)]
pub struct ModelsConfigUpdate {
    pub chat: ConfigSectionUpdate,
    pub tts: TtsConfigUpdate,
    #[serde(default)]
    pub speech_recognition: SpeechRecognitionConfigUpdate,
    #[serde(default)]
    pub audio_understanding: ConfigSectionUpdate,
    #[serde(default)]
    pub voice_input: VoiceInputConfigUpdate,
}

/// 单段模型配置更新请求体。
#[derive(Default, Deserialize)]
pub struct ConfigSectionUpdate {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_protocol: String,
    /// 空值或缺省表示保持原值；非空字符串表示更新。
    #[serde(default)]
    pub api_key: Option<String>,
    /// v1 统一密钥动作；存在时优先于旧 `api_key` 字段。
    #[serde(default)]
    pub api_key_update: SecretUpdate,
    #[serde(default)]
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub voice_id: Option<String>,
    pub speed: Option<f32>,
}

/// 语音合成配置更新请求体。
#[derive(Default, Deserialize)]
pub struct TtsConfigUpdate {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    /// 空值或缺省表示保持原值；非空字符串表示更新。
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_update: SecretUpdate,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub voice_id: String,
    pub speed: Option<f32>,
    #[serde(default)]
    pub response_format: String,
}

/// 语音输入策略更新请求体。
#[derive(Default, Deserialize)]
pub struct VoiceInputConfigUpdate {
    #[serde(default)]
    pub mode: String,
}

/// OpenAI-compatible 语音识别配置更新请求体。
#[derive(Default, Deserialize)]
pub struct SpeechRecognitionConfigUpdate {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_update: SecretUpdate,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub response_format: String,
}
