use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::{require_non_empty, require_opaque_token, require_rfc3339};
use super::{
    MemoryCategory, MemoryChangeType, MemoryError, MemoryErrorCode, MemoryFacet, MemoryId,
    MemoryImportance, MemoryRevisionId,
};

/// 模型查询原文的硬上限；必须在 Unicode 规范化前检查。
pub const MAX_MEMORY_QUERY_BYTES: usize = 384;
pub const MAX_MEMORY_QUERY_CHARS: usize = 120;
/// 单条原子记忆正文的硬上限；第一门与 Repository 门必须使用同一组数值。
pub const MAX_MEMORY_CONTENT_BYTES: usize = 4 * 1024;
pub const MAX_MEMORY_CONTENT_CHARS: usize = 1024;
/// revision 变化原因的硬上限。
pub const MAX_MEMORY_CHANGE_REASON_BYTES: usize = 1024;
pub const MAX_MEMORY_CHANGE_REASON_CHARS: usize = 256;
pub const MAX_MEMORY_SOURCE_QUOTE_BYTES: usize = 2 * 1024;
pub const MAX_MEMORY_SOURCE_QUOTE_CHARS: usize = 512;
pub const MAX_MEMORY_KEYWORD_BYTES: usize = 128;
pub const MAX_MEMORY_KEYWORD_CHARS: usize = 32;
pub const MAX_MEMORY_KEYWORDS: usize = 8;
pub const MAX_MEMORY_MUTATIONS: usize = 6;

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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<MemoryFacet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
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
        if self.query.is_empty()
            && self.facets.is_empty()
            && self.keywords.is_empty()
            && self.memory_id.is_none()
        {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }
        if !self.query.is_empty() {
            validate_memory_query_text(&self.query)?;
        }
        if self.facets.len() > MAX_MEMORY_KEYWORDS || self.keywords.len() > MAX_MEMORY_KEYWORDS {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }
        let mut unique_facets = std::collections::BTreeSet::new();
        if self
            .facets
            .iter()
            .any(|facet| !unique_facets.insert(*facet))
        {
            return Err(MemoryError::new(MemoryErrorCode::QueryRejected));
        }
        validate_memory_keywords(&self.keywords, MemoryErrorCode::QueryRejected, true)?;
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
        validate_memory_content(self.content())?;
        validate_memory_change_reason(self.change_reason())?;
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

/// 模型对普通事实是否需要用户确认的显式判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConfirmationMode {
    NotRequired,
    AskUser,
}

const fn default_memory_confirmation_mode() -> MemoryConfirmationMode {
    MemoryConfirmationMode::NotRequired
}

/// 批量写入中的单条候选。
///
/// 保持普通 object 字段，operation 的互斥约束由运行时执行，避免把 `oneOf`
/// 暴露给只接受顶层 object 的 OpenAI-compatible 上游。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMutationProposal {
    pub operation: MemoryChangeType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_quote: Option<String>,
    pub category: MemoryCategory,
    pub facet: MemoryFacet,
    pub keywords: Vec<String>,
    pub content: String,
    pub importance: MemoryImportance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time: Option<String>,
    pub change_reason: String,
    #[serde(default = "default_memory_confirmation_mode")]
    pub confirmation: MemoryConfirmationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<MemoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision_id: Option<MemoryRevisionId>,
}

impl MemoryMutationProposal {
    pub fn validate(&self) -> Result<(), MemoryError> {
        validate_memory_content(&self.content)?;
        validate_memory_change_reason(&self.change_reason)?;
        if let Some(source_quote) = &self.source_quote {
            validate_memory_source_quote(source_quote)?;
        }
        validate_memory_keywords(&self.keywords, MemoryErrorCode::InvalidRequest, false)?;
        if !self.facet.is_compatible_with(self.category) {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        if let Some(event_time) = &self.event_time {
            require_rfc3339(event_time)?;
        }
        match self.operation {
            MemoryChangeType::Create
                if self.memory_id.is_none() && self.expected_revision_id.is_none() => {}
            MemoryChangeType::Update | MemoryChangeType::Correct
                if self.memory_id.is_some() && self.expected_revision_id.is_some() =>
            {
                require_non_empty(&self.memory_id.as_ref().expect("已检查存在").0)?;
                require_non_empty(&self.expected_revision_id.as_ref().expect("已检查存在").0)?;
            }
            _ => return Err(MemoryError::new(MemoryErrorCode::InvalidRequest)),
        }
        Ok(())
    }

    pub fn to_params(&self) -> Result<MemoryMutateParams, MemoryError> {
        self.validate()?;
        Ok(match self.operation {
            MemoryChangeType::Create => MemoryMutateParams::Create {
                category: self.category,
                content: self.content.clone(),
                importance: self.importance,
                event_time: self.event_time.clone(),
                change_reason: self.change_reason.clone(),
            },
            MemoryChangeType::Update => MemoryMutateParams::Update {
                memory_id: self.memory_id.clone().expect("validate 已检查 memory_id"),
                expected_revision_id: self
                    .expected_revision_id
                    .clone()
                    .expect("validate 已检查 expected_revision_id"),
                category: self.category,
                content: self.content.clone(),
                importance: self.importance,
                event_time: self.event_time.clone(),
                change_reason: self.change_reason.clone(),
            },
            MemoryChangeType::Correct => MemoryMutateParams::Correct {
                memory_id: self.memory_id.clone().expect("validate 已检查 memory_id"),
                expected_revision_id: self
                    .expected_revision_id
                    .clone()
                    .expect("validate 已检查 expected_revision_id"),
                category: self.category,
                content: self.content.clone(),
                importance: self.importance,
                event_time: self.event_time.clone(),
                change_reason: self.change_reason.clone(),
            },
        })
    }

    fn from_legacy(params: MemoryMutateParams) -> Self {
        let category = params.category();
        Self {
            operation: params.operation(),
            source_quote: None,
            category,
            facet: MemoryFacet::default_for_category(category),
            keywords: vec![legacy_keyword(category).to_string()],
            content: params.content().to_string(),
            importance: params.importance(),
            event_time: params.event_time().map(str::to_string),
            change_reason: params.change_reason().to_string(),
            confirmation: MemoryConfirmationMode::NotRequired,
            memory_id: params.memory_id().cloned(),
            expected_revision_id: params.expected_revision_id().cloned(),
        }
    }
}

fn legacy_keyword(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::UserFact => "用户事实",
        MemoryCategory::UserPreference => "用户偏好",
        MemoryCategory::SharedExperience => "共同经历",
        MemoryCategory::Commitment => "约定",
        MemoryCategory::StoryState => "剧情状态",
    }
}

/// `memory_mutate` 的批量顶层 object；反序列化继续接受旧单条对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMutateRequest {
    pub mutations: Vec<MemoryMutationProposal>,
}

impl<'de> Deserialize<'de> for MemoryMutateRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("mutations").is_some() {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Batch {
                mutations: Vec<MemoryMutationProposal>,
            }
            let batch = Batch::deserialize(value).map_err(serde::de::Error::custom)?;
            return Ok(Self {
                mutations: batch.mutations,
            });
        }
        let legacy = MemoryMutateParams::deserialize(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            mutations: vec![MemoryMutationProposal::from_legacy(legacy)],
        })
    }
}

impl MemoryMutateRequest {
    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.mutations.is_empty() || self.mutations.len() > MAX_MEMORY_MUTATIONS {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        for mutation in &self.mutations {
            mutation.validate()?;
        }
        Ok(())
    }
}

pub(crate) fn validate_memory_query_text(value: &str) -> Result<(), MemoryError> {
    validate_bounded_text(
        value,
        MAX_MEMORY_QUERY_BYTES,
        MAX_MEMORY_QUERY_CHARS,
        MemoryErrorCode::QueryRejected,
    )
}

pub(crate) fn validate_memory_content(value: &str) -> Result<(), MemoryError> {
    validate_bounded_text(
        value,
        MAX_MEMORY_CONTENT_BYTES,
        MAX_MEMORY_CONTENT_CHARS,
        MemoryErrorCode::InvalidRequest,
    )
}

pub(crate) fn validate_memory_change_reason(value: &str) -> Result<(), MemoryError> {
    validate_bounded_text(
        value,
        MAX_MEMORY_CHANGE_REASON_BYTES,
        MAX_MEMORY_CHANGE_REASON_CHARS,
        MemoryErrorCode::InvalidRequest,
    )
}

pub(crate) fn validate_memory_source_quote(value: &str) -> Result<(), MemoryError> {
    validate_bounded_text(
        value,
        MAX_MEMORY_SOURCE_QUOTE_BYTES,
        MAX_MEMORY_SOURCE_QUOTE_CHARS,
        MemoryErrorCode::SourceIneligible,
    )
}

pub(crate) fn validate_memory_keyword(value: &str) -> Result<(), MemoryError> {
    validate_bounded_text(
        value,
        MAX_MEMORY_KEYWORD_BYTES,
        MAX_MEMORY_KEYWORD_CHARS,
        MemoryErrorCode::InvalidRequest,
    )
}

fn validate_memory_keywords(
    keywords: &[String],
    error_code: MemoryErrorCode,
    allow_empty: bool,
) -> Result<(), MemoryError> {
    if (!allow_empty && keywords.is_empty()) || keywords.len() > MAX_MEMORY_KEYWORDS {
        return Err(MemoryError::new(error_code));
    }
    let mut normalized = std::collections::BTreeSet::new();
    for keyword in keywords {
        validate_memory_keyword(keyword).map_err(|_| MemoryError::new(error_code))?;
        let canonical = keyword.trim().to_lowercase();
        if !normalized.insert(canonical) {
            return Err(MemoryError::new(error_code));
        }
    }
    Ok(())
}

fn validate_bounded_text(
    value: &str,
    max_bytes: usize,
    max_chars: usize,
    error_code: MemoryErrorCode,
) -> Result<(), MemoryError> {
    if value.len() > max_bytes || value.chars().count() > max_chars {
        return Err(MemoryError::new(error_code));
    }
    require_non_empty(value).map_err(|_| MemoryError::new(error_code))
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
    pub facet: MemoryFacet,
    pub keywords: Vec<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMutationItemState {
    Staged,
    ConfirmationRequired,
    Rejected,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMutationItemReceipt {
    pub index: usize,
    pub state: MemoryMutationItemState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<MemoryChangeType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<MemoryId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<MemoryRevisionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMutationBatchState {
    Staged,
    Partial,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMutationBatchReceipt {
    pub state: MemoryMutationBatchState,
    pub staged_count: usize,
    pub confirmation_required_count: usize,
    pub rejected_count: usize,
    pub skipped_count: usize,
    pub items: Vec<MemoryMutationItemReceipt>,
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
