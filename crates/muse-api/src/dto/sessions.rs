//! 会话接口的请求与响应 DTO。

use muse_core::domain::conversation::Message;
use muse_core::domain::usage::RuntimeContextSnapshot;
use serde::{Deserialize, Serialize};

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
