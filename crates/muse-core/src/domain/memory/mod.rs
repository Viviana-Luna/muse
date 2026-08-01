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
    MemoryCategory, MemoryChangeType, MemoryEntry, MemoryEntryState, MemoryId, MemoryImportance,
    MemoryRecord, MemoryRevision, MemoryRevisionId, MemoryRevisionState, MemorySourceEvidence,
    MemorySourceKind,
};
pub use ports::{
    MemoryDeletionAuthority, MemoryDeletionAuthorityReceipt, MemoryDeletionAuthorityRequest,
    MemoryDeletionCheckRequest, MemoryDeletionDecision, MemoryDeletionSubject, MemoryDerivationKey,
    MemoryRepository, MemoryRetriever, MemorySafetyAssessment, MemorySafetyFailure,
    MemorySafetyStage, MemorySensitivityPolicy, MemorySensitivityRequest,
};
pub use runtime_binding::{
    ConfirmedMemoryDeleteRequest, MemoryCommitEnvelope, MemoryDeleteConfirmation,
    MemoryDeleteConfirmationSource, MemoryMutationTransition, MemoryPersonaScope,
    MemoryRetrievalRequest, MemoryRuntimeBinding, MemorySourceEligibility, MemoryStagedMutation,
};
pub use tool_request::{
    MEMORY_DELETE_TOOL_NAME, MEMORY_MUTATE_TOOL_NAME, MEMORY_QUERY_TOOL_NAME,
    MemoryBatchCommitReceipt, MemoryCursor, MemoryDeleteParams, MemoryDeleteReceipt,
    MemoryMutateParams, MemoryMutationReceipt, MemoryMutationReceiptState, MemoryQueryItem,
    MemoryQueryPageReceipt, MemoryQueryParams,
};

#[cfg(test)]
mod tests;
