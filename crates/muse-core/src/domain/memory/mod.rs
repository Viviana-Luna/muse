//! Persona 长期记忆的领域契约。
//!
//! 本模块只冻结领域语义、模型参数与运行时端口，不注册 Tool，也不实现存储。

mod error;
mod management;
mod model;
mod ports;
mod runtime_binding;
mod tool_request;

pub use error::{MemoryError, MemoryErrorCode};
pub use management::{
    MemoryImportanceAdjustment, MemoryImportanceAdjustmentReceipt, MemoryManagementAuthorization,
    MemoryManagementBinding, MemoryManagementContentMutation, MemoryManagementContentParams,
};
pub use model::{
    MemoryCategory, MemoryChangeType, MemoryEntry, MemoryEntryState, MemoryFacet, MemoryId,
    MemoryImportance, MemoryRecord, MemoryRevision, MemoryRevisionId, MemoryRevisionState,
    MemorySourceEvidence, MemorySourceKind,
};
pub use ports::{
    MAX_MEMORY_DELETION_FIELD_BYTES, MAX_MEMORY_DELETION_SUBJECTS, MemoryDeletionAuthority,
    MemoryDeletionAuthorityReceipt, MemoryDeletionAuthorityRequest, MemoryDeletionCheckRequest,
    MemoryDeletionDecision, MemoryDeletionSubject, MemoryDerivationKey, MemoryRepository,
    MemoryRetriever, MemorySafetyAssessment, MemorySafetyFailure, MemorySafetyStage,
    MemorySensitivityPolicy, MemorySensitivityRequest,
};
pub use runtime_binding::{
    ConfirmedMemoryDeleteRequest, MemoryCommitEnvelope, MemoryDeleteConfirmation,
    MemoryDeleteConfirmationSource, MemoryMutationTransition, MemoryPersonaScope,
    MemoryRetrievalFilters, MemoryRetrievalRequest, MemoryRetrievalTurn, MemoryRuntimeBinding,
    MemorySourceEligibility, MemoryStagedMutation,
};
pub use tool_request::{
    MAX_MEMORY_CHANGE_REASON_BYTES, MAX_MEMORY_CHANGE_REASON_CHARS, MAX_MEMORY_CONTENT_BYTES,
    MAX_MEMORY_CONTENT_CHARS, MAX_MEMORY_KEYWORD_BYTES, MAX_MEMORY_KEYWORD_CHARS,
    MAX_MEMORY_KEYWORDS, MAX_MEMORY_MUTATIONS, MAX_MEMORY_QUERY_BYTES, MAX_MEMORY_QUERY_CHARS,
    MAX_MEMORY_SOURCE_QUOTE_BYTES, MAX_MEMORY_SOURCE_QUOTE_CHARS, MEMORY_DELETE_TOOL_NAME,
    MEMORY_MUTATE_TOOL_NAME, MEMORY_QUERY_TOOL_NAME, MemoryBatchCommitReceipt,
    MemoryConfirmationMode, MemoryCursor, MemoryDeleteParams, MemoryDeleteReceipt,
    MemoryMutateParams, MemoryMutateRequest, MemoryMutationBatchReceipt, MemoryMutationBatchState,
    MemoryMutationItemReceipt, MemoryMutationItemState, MemoryMutationProposal,
    MemoryMutationReceipt, MemoryMutationReceiptState, MemoryQueryItem, MemoryQueryPageReceipt,
    MemoryQueryParams,
};
pub(crate) use tool_request::{
    validate_memory_change_reason, validate_memory_content, validate_memory_keyword,
    validate_memory_query_text, validate_memory_source_quote,
};

#[cfg(test)]
mod tests;
