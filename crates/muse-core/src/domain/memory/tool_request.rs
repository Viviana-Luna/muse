use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::{require_non_empty, require_opaque_token, require_rfc3339};
use super::{
    MemoryCategory, MemoryChangeType, MemoryError, MemoryErrorCode, MemoryId, MemoryImportance,
    MemoryRevisionId,
};

/// 记忆工具使用的稳定名称。
pub const MEMORY_QUERY_TOOL_NAME: &str = "memory_query";
pub const MEMORY_MUTATE_TOOL_NAME: &str = "memory_mutate";
pub const MEMORY_DELETE_TOOL_NAME: &str = "memory_delete";

/// 不透明查询游标；调用方只能原样回传，不能解析或拼装。
#[derive(Clone, PartialEq, Eq)]
pub struct MemoryCursor(String);

impl MemoryCursor {
    pub fn from_runtime(value: impl Into<String>) -> Result<Self, MemoryError> {
        let value = value.into();
        require_opaque_token(&value)
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidCursor))?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MemoryCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemoryCursor([opaque])")
    }
}

impl Serialize for MemoryCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MemoryCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_runtime(value).map_err(serde::de::Error::custom)
    }
}

/// 模型可传的 `memory_query` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryQueryParams {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<MemoryCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<MemoryId>,
    #[serde(default)]
    pub include_history: bool,
}

impl MemoryQueryParams {
    pub fn validate(&self) -> Result<(), MemoryError> {
        require_non_empty(&self.query)?;
        if self.limit == Some(0) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        if let Some(cursor) = &self.cursor {
            require_opaque_token(cursor.as_str())
                .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidCursor))?;
        }
        if let Some(as_of) = &self.as_of {
            require_rfc3339(as_of)?;
        }
        if let Some(memory_id) = &self.memory_id {
            require_non_empty(&memory_id.0)?;
        }
        Ok(())
    }
}

/// 模型可传的 `memory_mutate` 参数。
///
/// 使用带标签枚举确保 create 无法携带旧标识，update/correct 则必须携带并发 revision。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryMutateParams {
    Create {
        category: MemoryCategory,
        content: String,
        importance: MemoryImportance,
        event_time: Option<String>,
        change_reason: String,
    },
    Update {
        memory_id: MemoryId,
        expected_revision_id: MemoryRevisionId,
        category: MemoryCategory,
        content: String,
        importance: MemoryImportance,
        event_time: Option<String>,
        change_reason: String,
    },
    Correct {
        memory_id: MemoryId,
        expected_revision_id: MemoryRevisionId,
        category: MemoryCategory,
        content: String,
        importance: MemoryImportance,
        event_time: Option<String>,
        change_reason: String,
    },
}

impl fmt::Debug for MemoryMutateParams {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryMutateParams")
            .field("operation", &self.operation())
            .field("category", &self.category())
            .field("importance", &self.importance())
            .field("has_event_time", &self.event_time().is_some())
            .field("content", &"[已去敏]")
            .field("change_reason", &"[已去敏]")
            .finish()
    }
}

impl MemoryMutateParams {
    pub const fn operation(&self) -> MemoryChangeType {
        match self {
            Self::Create { .. } => MemoryChangeType::Create,
            Self::Update { .. } => MemoryChangeType::Update,
            Self::Correct { .. } => MemoryChangeType::Correct,
        }
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        require_non_empty(self.content())?;
        require_non_empty(self.change_reason())?;
        if let Some(memory_id) = self.memory_id() {
            require_non_empty(&memory_id.0)?;
        }
        if let Some(revision_id) = self.expected_revision_id() {
            require_non_empty(&revision_id.0)?;
        }
        if let Some(event_time) = self.event_time() {
            require_rfc3339(event_time)?;
        }
        Ok(())
    }

    pub const fn category(&self) -> MemoryCategory {
        match self {
            Self::Create { category, .. }
            | Self::Update { category, .. }
            | Self::Correct { category, .. } => *category,
        }
    }

    pub const fn importance(&self) -> MemoryImportance {
        match self {
            Self::Create { importance, .. }
            | Self::Update { importance, .. }
            | Self::Correct { importance, .. } => *importance,
        }
    }

    pub fn content(&self) -> &str {
        match self {
            Self::Create { content, .. }
            | Self::Update { content, .. }
            | Self::Correct { content, .. } => content,
        }
    }

    pub fn event_time(&self) -> Option<&str> {
        match self {
            Self::Create { event_time, .. }
            | Self::Update { event_time, .. }
            | Self::Correct { event_time, .. } => event_time.as_deref(),
        }
    }

    pub fn change_reason(&self) -> &str {
        match self {
            Self::Create { change_reason, .. }
            | Self::Update { change_reason, .. }
            | Self::Correct { change_reason, .. } => change_reason,
        }
    }

    pub fn memory_id(&self) -> Option<&MemoryId> {
        match self {
            Self::Create { .. } => None,
            Self::Update { memory_id, .. } | Self::Correct { memory_id, .. } => Some(memory_id),
        }
    }

    pub fn expected_revision_id(&self) -> Option<&MemoryRevisionId> {
        match self {
            Self::Create { .. } => None,
            Self::Update {
                expected_revision_id,
                ..
            }
            | Self::Correct {
                expected_revision_id,
                ..
            } => Some(expected_revision_id),
        }
    }
}

/// 模型可传的 `memory_delete` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryDeleteParams {
    Memory { memory_id: MemoryId },
    PersonaAll,
}

impl MemoryDeleteParams {
    pub fn validate(&self) -> Result<(), MemoryError> {
        if let Self::Memory { memory_id } = self {
            require_non_empty(&memory_id.0)?;
        }
        Ok(())
    }
}

/// 查询返回给模型的单条紧凑内容。
///
/// 历史查询需要变化类型与原因；Retriever 仍不得构造 corrected revision 的条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryQueryItem {
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
    pub category: MemoryCategory,
    pub content: String,
    pub importance: MemoryImportance,
    pub event_time: Option<String>,
    pub recorded_at: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub change_type: MemoryChangeType,
    pub change_reason: String,
}

/// 一页查询收据；具体页大小和 Token 预算由后续运行时冻结。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryQueryPageReceipt {
    pub items: Vec<MemoryQueryItem>,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<MemoryCursor>,
}

impl MemoryQueryPageReceipt {
    pub fn new(items: Vec<MemoryQueryItem>, next_cursor: Option<MemoryCursor>) -> Self {
        Self {
            has_more: next_cursor.is_some(),
            items,
            next_cursor,
        }
    }
}

/// 变更收据状态，明确区分 Turn 暂存与真正 durable。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMutationReceiptState {
    Staged,
    Durable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMutationReceipt {
    pub operation: MemoryChangeType,
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
    pub state: MemoryMutationReceiptState,
}

/// 同一 committed Turn 的原子批量提交收据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBatchCommitReceipt {
    pub idempotency_key: String,
    pub mutations: Vec<MemoryMutationReceipt>,
    pub durable_at: String,
}

/// 删除成功收据只代表完整删除链 durable 成功。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryDeleteReceipt {
    pub deletion_id: String,
    pub deleted_memory_count: u64,
    pub completed_at: String,
}
