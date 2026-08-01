//! Persona 长期记忆管理接口的请求与响应 DTO。

use muse_core::domain::memory::{
    MemoryCategory, MemoryChangeType, MemoryEntry, MemoryId, MemoryImportance, MemoryRevision,
    MemoryRevisionId, MemoryRevisionState,
};
use serde::{Deserialize, Serialize};

/// 记忆列表查询参数。
///
/// 管理读取与模型读取共用检索语义：`query` 必填非空，`category`/`importance`
/// 为页内过滤条件，`cursor` 只能原样回传上一页收据中的不透明游标。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryListQuery {
    pub query: String,
    #[serde(default)]
    pub category: Option<MemoryCategory>,
    #[serde(default)]
    pub importance: Option<MemoryImportance>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// 手工新增记忆请求体。
///
/// `operation_id` 可选；客户端携带时同一 operation 重放会被 Repository 幂等
/// 识别或稳定拒绝，绝不产生重复记忆。缺省时由服务端生成。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCreateRequest {
    pub category: MemoryCategory,
    pub content: String,
    pub importance: MemoryImportance,
    #[serde(default)]
    pub event_time: Option<String>,
    pub change_reason: String,
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// 纠正记忆请求体；memory_id 只来自路径，请求体不能跨记忆写入。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCorrectRequest {
    pub expected_revision_id: MemoryRevisionId,
    pub category: MemoryCategory,
    pub content: String,
    #[serde(default)]
    pub event_time: Option<String>,
    pub change_reason: String,
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// 重要程度调整请求体；只更新逻辑 entry，不创建内容 revision。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryImportanceAdjustRequest {
    pub expected_revision_id: MemoryRevisionId,
    pub expected_importance: MemoryImportance,
    pub importance: MemoryImportance,
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// 删除请求体；`operation_id` 必须由客户端在一次操作的所有重试中保持不变。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryDeleteRequest {
    pub operation_id: String,
}

/// 记忆详情响应体。
///
/// 来源跳转只暴露 conversation/turn 索引，不复制任何聊天正文；管理来源
/// （手工新增、纠正）没有对话索引，两个字段为 `null`。
#[derive(Serialize)]
pub struct MemoryDetailResponse {
    pub entry: MemoryEntry,
    pub current_revision: MemoryRevisionResponse,
    pub source_conversation_id: Option<String>,
    pub source_turn_id: Option<String>,
}

/// 管理 API 可见的 revision。
///
/// 领域 `MemorySourceEvidence` 还携带管理操作 ID、授权时间和对话来源 kind；这些
/// 均不属于来源跳转契约，因此响应只保留 conversation/turn 两个稳定索引。
#[derive(Serialize)]
pub struct MemoryRevisionResponse {
    pub revision_id: MemoryRevisionId,
    pub memory_id: MemoryId,
    pub content: String,
    pub event_time: Option<String>,
    pub recorded_at: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub change_type: MemoryChangeType,
    pub change_reason: String,
    pub source_conversation_id: Option<String>,
    pub source_turn_id: Option<String>,
    pub safety_policy_version: String,
    pub state: MemoryRevisionState,
}

impl From<MemoryRevision> for MemoryRevisionResponse {
    fn from(revision: MemoryRevision) -> Self {
        Self {
            revision_id: revision.revision_id,
            memory_id: revision.memory_id,
            content: revision.content,
            event_time: revision.event_time,
            recorded_at: revision.recorded_at,
            valid_from: revision.valid_from,
            valid_to: revision.valid_to,
            change_type: revision.change_type,
            change_reason: revision.change_reason,
            source_conversation_id: revision.source.conversation_id().map(str::to_string),
            source_turn_id: revision.source.turn_id().map(str::to_string),
            safety_policy_version: revision.safety_policy_version,
            state: revision.state,
        }
    }
}

/// 版本历史响应体；含 corrected 审计条目，只允许管理入口读取。
#[derive(Serialize)]
pub struct MemoryHistoryResponse {
    pub memory_id: MemoryId,
    pub revisions: Vec<MemoryRevisionResponse>,
}
