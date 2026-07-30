use std::fmt;

use serde::{Deserialize, Serialize};

/// 可稳定匹配的领域错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MemoryErrorCode {
    #[serde(rename = "memory_invalid_request")]
    InvalidRequest,
    #[serde(rename = "memory_invalid_state_transition")]
    InvalidStateTransition,
    #[serde(rename = "memory_not_found")]
    MemoryNotFound,
    #[serde(rename = "memory_revision_conflict")]
    RevisionConflict,
    #[serde(rename = "memory_persona_scope_mismatch")]
    PersonaScopeMismatch,
    #[serde(rename = "memory_source_ineligible")]
    SourceIneligible,
    #[serde(rename = "memory_sensitive_content_rejected")]
    SensitiveContentRejected,
    #[serde(rename = "memory_sensitivity_unavailable")]
    SensitivityUnavailable,
    #[serde(rename = "memory_cursor_invalid")]
    InvalidCursor,
    #[serde(rename = "memory_cursor_expired")]
    CursorExpired,
    #[serde(rename = "memory_query_rejected")]
    QueryRejected,
    #[serde(rename = "memory_query_budget_exceeded")]
    QueryBudgetExceeded,
    #[serde(rename = "memory_delete_confirmation_required")]
    DeleteConfirmationRequired,
    #[serde(rename = "memory_deletion_authority_unavailable")]
    DeletionAuthorityUnavailable,
    #[serde(rename = "memory_deletion_incomplete")]
    DeletionIncomplete,
    #[serde(rename = "memory_repository_unavailable")]
    RepositoryUnavailable,
}

impl MemoryErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "memory_invalid_request",
            Self::InvalidStateTransition => "memory_invalid_state_transition",
            Self::MemoryNotFound => "memory_not_found",
            Self::RevisionConflict => "memory_revision_conflict",
            Self::PersonaScopeMismatch => "memory_persona_scope_mismatch",
            Self::SourceIneligible => "memory_source_ineligible",
            Self::SensitiveContentRejected => "memory_sensitive_content_rejected",
            Self::SensitivityUnavailable => "memory_sensitivity_unavailable",
            Self::InvalidCursor => "memory_cursor_invalid",
            Self::CursorExpired => "memory_cursor_expired",
            Self::QueryRejected => "memory_query_rejected",
            Self::QueryBudgetExceeded => "memory_query_budget_exceeded",
            Self::DeleteConfirmationRequired => "memory_delete_confirmation_required",
            Self::DeletionAuthorityUnavailable => "memory_deletion_authority_unavailable",
            Self::DeletionIncomplete => "memory_deletion_incomplete",
            Self::RepositoryUnavailable => "memory_repository_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryError {
    code: MemoryErrorCode,
}

impl MemoryError {
    pub const fn new(code: MemoryErrorCode) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> MemoryErrorCode {
        self.code
    }

    pub const fn stable_code(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.code {
            MemoryErrorCode::InvalidRequest => "记忆请求参数无效。",
            MemoryErrorCode::InvalidStateTransition => "记忆状态转换无效。",
            MemoryErrorCode::MemoryNotFound => "指定记忆不存在。",
            MemoryErrorCode::RevisionConflict => "记忆 revision 已变化，请刷新后重试。",
            MemoryErrorCode::PersonaScopeMismatch => "记忆不属于当前 Persona。",
            MemoryErrorCode::SourceIneligible => "当前来源不具备形成记忆的资格。",
            MemoryErrorCode::SensitiveContentRejected => "该内容不允许进入长期记忆。",
            MemoryErrorCode::SensitivityUnavailable => "敏感判定不可用，已拒绝持久化。",
            MemoryErrorCode::InvalidCursor => "记忆查询游标无效。",
            MemoryErrorCode::CursorExpired => "记忆查询游标已过期。",
            MemoryErrorCode::QueryRejected => "记忆查询不满足检索要求。",
            MemoryErrorCode::QueryBudgetExceeded => "本轮记忆查询预算已用尽。",
            MemoryErrorCode::DeleteConfirmationRequired => "删除记忆前需要专用用户确认。",
            MemoryErrorCode::DeletionAuthorityUnavailable => "记忆删除权威不可用。",
            MemoryErrorCode::DeletionIncomplete => "记忆删除未完整完成。",
            MemoryErrorCode::RepositoryUnavailable => "记忆 Repository 当前不可用。",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MemoryError {}

pub(crate) fn require_non_empty(value: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    Ok(())
}

pub(crate) fn require_rfc3339(value: &str) -> Result<(), MemoryError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))
}

pub(crate) fn require_opaque_token(value: &str) -> Result<(), MemoryError> {
    require_non_empty(value)?;
    if value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    Ok(())
}
