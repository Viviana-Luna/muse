use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::{require_non_empty, require_opaque_token, require_rfc3339};
use super::{
    ConfirmedMemoryDeleteRequest, MemoryBatchCommitReceipt, MemoryCategory, MemoryChangeType,
    MemoryCommitEnvelope, MemoryDeleteReceipt, MemoryError, MemoryErrorCode, MemoryFacet, MemoryId,
    MemoryImportance, MemoryImportanceAdjustment, MemoryImportanceAdjustmentReceipt,
    MemoryManagementContentMutation, MemoryMutationReceipt, MemoryQueryPageReceipt, MemoryRecord,
    MemoryRetrievalRequest, MemoryRevisionId, MemorySourceEvidence,
};

/// 敏感判断所处的双门阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySafetyStage {
    TurnStaging,
    RepositoryCommit,
}

/// 敏感检测无法得出允许结论时的封闭原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySafetyFailure {
    DetectorUnavailable,
    DecisionMissing,
    Indeterminate,
    PolicyUnavailable,
}

/// 敏感判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySafetyAssessment {
    Allowed {
        stage: MemorySafetyStage,
        policy_version: String,
    },
    Rejected {
        stage: MemorySafetyStage,
        policy_version: Option<String>,
    },
    FailClosed {
        stage: MemorySafetyStage,
        reason: MemorySafetyFailure,
    },
}

impl MemorySafetyAssessment {
    pub const fn permits_staging(&self) -> bool {
        matches!(
            self,
            Self::Allowed {
                stage: MemorySafetyStage::TurnStaging,
                ..
            }
        )
    }

    pub const fn permits_persistence(&self) -> bool {
        matches!(
            self,
            Self::Allowed {
                stage: MemorySafetyStage::RepositoryCommit,
                ..
            }
        )
    }

    pub(super) fn into_staging_permit(
        self,
        request: MemorySensitivityRequest<'_>,
    ) -> Result<MemoryStagingPermit, MemoryError> {
        match self {
            Self::Allowed {
                stage: MemorySafetyStage::TurnStaging,
                policy_version,
            } => {
                if request.stage != MemorySafetyStage::TurnStaging {
                    return Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable));
                }
                require_non_empty(&policy_version)?;
                Ok(MemoryStagingPermit {
                    policy_version,
                    identity: MemorySensitivityIdentity::from(request),
                })
            }
            Self::Rejected { .. } => {
                Err(MemoryError::new(MemoryErrorCode::SensitiveContentRejected))
            }
            Self::FailClosed { .. }
            | Self::Allowed {
                stage: MemorySafetyStage::RepositoryCommit,
                ..
            } => Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable)),
        }
    }

    pub(super) fn into_repository_permit(
        self,
        request: MemorySensitivityRequest<'_>,
    ) -> Result<MemoryPersistencePermit, MemoryError> {
        match self {
            Self::Allowed {
                stage: MemorySafetyStage::RepositoryCommit,
                policy_version,
            } => {
                if request.stage != MemorySafetyStage::RepositoryCommit {
                    return Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable));
                }
                require_non_empty(&policy_version)?;
                Ok(MemoryPersistencePermit {
                    policy_version,
                    identity: MemorySensitivityIdentity::from(request),
                })
            }
            Self::Rejected { .. } => {
                Err(MemoryError::new(MemoryErrorCode::SensitiveContentRejected))
            }
            Self::FailClosed { .. }
            | Self::Allowed {
                stage: MemorySafetyStage::TurnStaging,
                ..
            } => Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable)),
        }
    }
}

/// 第一层敏感门签发的私有暂存许可。
#[derive(Clone, PartialEq, Eq)]
pub(super) struct MemoryStagingPermit {
    policy_version: String,
    identity: MemorySensitivityIdentity,
}

impl MemoryStagingPermit {
    pub(super) fn policy_version(&self) -> &str {
        &self.policy_version
    }

    pub(super) fn validate(
        &self,
        request: MemorySensitivityRequest<'_>,
    ) -> Result<(), MemoryError> {
        validate_permit_identity(&self.identity, request)
    }
}

impl fmt::Debug for MemoryStagingPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemoryStagingPermit([已绑定并去敏])")
    }
}

/// Repository 最终敏感门签发的私有持久化许可。
#[derive(Clone, PartialEq, Eq)]
pub(super) struct MemoryPersistencePermit {
    policy_version: String,
    identity: MemorySensitivityIdentity,
}

impl MemoryPersistencePermit {
    pub(super) fn policy_version(&self) -> &str {
        &self.policy_version
    }

    pub(super) fn validate(
        &self,
        request: MemorySensitivityRequest<'_>,
    ) -> Result<(), MemoryError> {
        validate_permit_identity(&self.identity, request)
    }
}

impl fmt::Debug for MemoryPersistencePermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemoryPersistencePermit([已绑定并去敏])")
    }
}

/// 敏感判定覆盖完整 mutation 身份、所有模型可控正文和运行时来源证据。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MemorySensitivityRequest<'a> {
    pub stage: MemorySafetyStage,
    pub operation_id: &'a str,
    pub operation: MemoryChangeType,
    pub memory_id: Option<&'a MemoryId>,
    pub expected_revision_id: Option<&'a MemoryRevisionId>,
    pub assigned_memory_id: &'a MemoryId,
    pub assigned_revision_id: &'a MemoryRevisionId,
    pub category: MemoryCategory,
    pub facet: MemoryFacet,
    pub keywords: &'a [String],
    pub importance: Option<MemoryImportance>,
    pub content: &'a str,
    pub change_reason: &'a str,
    pub event_time: Option<&'a str>,
    pub source: &'a MemorySourceEvidence,
}

impl fmt::Debug for MemorySensitivityRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemorySensitivityRequest")
            .field("stage", &self.stage)
            .field("operation", &self.operation)
            .field("category", &self.category)
            .field("facet", &self.facet)
            .field("keyword_count", &self.keywords.len())
            .field("importance", &self.importance)
            .field("has_event_time", &self.event_time.is_some())
            .field("bound_identity", &"[已去敏]")
            .finish()
    }
}

pub trait MemorySensitivityPolicy: Send + Sync {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment;
}

#[derive(Clone, PartialEq, Eq)]
struct MemorySensitivityIdentity {
    stage: MemorySafetyStage,
    operation_id: String,
    operation: MemoryChangeType,
    memory_id: Option<MemoryId>,
    expected_revision_id: Option<MemoryRevisionId>,
    assigned_memory_id: MemoryId,
    assigned_revision_id: MemoryRevisionId,
    category: MemoryCategory,
    facet: MemoryFacet,
    keywords: Vec<String>,
    importance: Option<MemoryImportance>,
    content: String,
    change_reason: String,
    event_time: Option<String>,
    source: MemorySourceEvidence,
}

impl From<MemorySensitivityRequest<'_>> for MemorySensitivityIdentity {
    fn from(request: MemorySensitivityRequest<'_>) -> Self {
        Self {
            stage: request.stage,
            operation_id: request.operation_id.to_string(),
            operation: request.operation,
            memory_id: request.memory_id.cloned(),
            expected_revision_id: request.expected_revision_id.cloned(),
            assigned_memory_id: request.assigned_memory_id.clone(),
            assigned_revision_id: request.assigned_revision_id.clone(),
            category: request.category,
            facet: request.facet,
            keywords: request.keywords.to_vec(),
            importance: request.importance,
            content: request.content.to_string(),
            change_reason: request.change_reason.to_string(),
            event_time: request.event_time.map(str::to_string),
            source: request.source.clone(),
        }
    }
}

fn validate_permit_identity(
    expected: &MemorySensitivityIdentity,
    request: MemorySensitivityRequest<'_>,
) -> Result<(), MemoryError> {
    if expected == &MemorySensitivityIdentity::from(request) {
        Ok(())
    } else {
        Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable))
    }
}

/// 删除权威中的固定长度派生摘要，不接受任意字符串或记忆正文。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryDerivationKey([u8; Self::DIGEST_LENGTH]);

impl MemoryDerivationKey {
    pub const DIGEST_LENGTH: usize = 32;

    /// 只接受上游受保护派生流程产生的固定长度 digest。
    pub const fn from_digest(digest: [u8; Self::DIGEST_LENGTH]) -> Self {
        Self(digest)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::DIGEST_LENGTH] {
        &self.0
    }
}

impl fmt::Debug for MemoryDerivationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemoryDerivationKey([受保护摘要])")
    }
}

impl Serialize for MemoryDerivationKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut encoded = String::with_capacity(Self::DIGEST_LENGTH * 2);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
        }
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for MemoryDerivationKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        decode_derivation_digest(&value)
            .map(Self::from_digest)
            .map_err(serde::de::Error::custom)
    }
}

/// 删除权威可阻断的范围。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryDeletionSubject {
    Persona {
        persona_id: String,
    },
    Memory {
        persona_id: String,
        memory_id: MemoryId,
    },
    SourceTurn {
        persona_id: String,
        conversation_id: String,
        turn_id: String,
    },
    Derivation {
        persona_id: String,
        derivation_key: MemoryDerivationKey,
    },
}

/// 单次删除恢复允许物化的最大 subject 数量。
///
/// 该上限与恢复读取的总行数门禁一致，既约束公开请求构造，也约束从独立
/// authority SQLite 恢复出的领域事件，避免旁路数据库绕过领域容量契约。
pub const MAX_MEMORY_DELETION_SUBJECTS: usize = 512;

/// 删除权威标识字段的最大 UTF-8 字节数。
pub const MAX_MEMORY_DELETION_FIELD_BYTES: usize = 1024;

impl MemoryDeletionSubject {
    pub fn validate(&self) -> Result<(), MemoryError> {
        match self {
            Self::Persona { persona_id } => validate_deletion_field(persona_id),
            Self::Memory {
                persona_id,
                memory_id,
            } => {
                validate_deletion_field(persona_id)?;
                validate_deletion_field(&memory_id.0)
            }
            Self::SourceTurn {
                persona_id,
                conversation_id,
                turn_id,
            } => {
                validate_deletion_field(persona_id)?;
                validate_deletion_field(conversation_id)?;
                validate_deletion_field(turn_id)
            }
            Self::Derivation {
                persona_id,
                derivation_key: _,
            } => {
                validate_deletion_field(persona_id)?;
                Ok(())
            }
        }
    }
}

/// 写入独立删除权威的请求，不包含任何记忆正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeletionAuthorityRequest {
    deletion_id: String,
    subjects: BTreeSet<MemoryDeletionSubject>,
    recorded_at: String,
}

impl MemoryDeletionAuthorityRequest {
    pub fn new(
        deletion_id: impl Into<String>,
        subjects: BTreeSet<MemoryDeletionSubject>,
        recorded_at: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let deletion_id = deletion_id.into();
        let recorded_at = recorded_at.into();
        require_opaque_token(&deletion_id)?;
        require_rfc3339(&recorded_at)?;
        validate_deletion_subjects(&subjects)?;
        Ok(Self {
            deletion_id,
            subjects,
            recorded_at,
        })
    }

    pub fn deletion_id(&self) -> &str {
        &self.deletion_id
    }

    pub fn subjects(&self) -> &BTreeSet<MemoryDeletionSubject> {
        &self.subjects
    }

    pub fn recorded_at(&self) -> &str {
        &self.recorded_at
    }
}

/// 删除权威完成 durable flush 后签发的收据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeletionAuthorityReceipt {
    pub deletion_id: String,
    pub authority_revision: String,
    pub subjects: BTreeSet<MemoryDeletionSubject>,
    pub durable_at: String,
}

/// 恢复、创建或派生前向删除权威发起的阻断检查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeletionCheckRequest {
    subjects: BTreeSet<MemoryDeletionSubject>,
}

impl MemoryDeletionCheckRequest {
    pub fn new(subjects: BTreeSet<MemoryDeletionSubject>) -> Result<Self, MemoryError> {
        validate_deletion_subjects(&subjects)?;
        Ok(Self { subjects })
    }

    pub fn subjects(&self) -> &BTreeSet<MemoryDeletionSubject> {
        &self.subjects
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryDeletionDecision {
    Allowed,
    Blocked {
        matched: BTreeSet<MemoryDeletionSubject>,
    },
}

/// 删除权威必须位于普通 runtime SQLite 备份之外的独立恢复域。
///
/// `check` 返回任何错误时，恢复、读取、创建和派生都必须 fail closed。
pub trait MemoryDeletionAuthority: Send + Sync {
    fn record(
        &self,
        request: &MemoryDeletionAuthorityRequest,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError>;

    fn check(
        &self,
        request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError>;
}

/// 可替换的检索端口。
///
/// 实现必须先硬过滤不相关候选，再按有效权重排序；游标必须绑定 Persona、
/// 查询条件、排序版本与真实 Turn，且不得返回 corrected、deleted 或跨 Persona 内容。
/// 同 Turn 同 cursor 重放必须幂等返回同一页；跨 Turn 使用必须稳定拒绝。
pub trait MemoryRetriever: Send + Sync {
    fn retrieve(
        &self,
        request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError>;
}

/// 记忆唯一写入端口。
///
/// `apply_committed_batch` 必须对同一 committed Turn 的全部暂存操作执行原子且幂等的
/// 批量提交，并在事务前逐项重跑 RepositoryCommit 敏感判定。`delete_confirmed`
/// 必须先 durable 写入独立删除权威，再清除主库明文。
pub trait MemoryRepository: Send + Sync {
    fn current(
        &self,
        scope: &super::MemoryPersonaScope,
        memory_id: &MemoryId,
    ) -> Result<Option<MemoryRecord>, MemoryError>;

    fn apply_committed_batch(
        &self,
        envelope: &MemoryCommitEnvelope,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryBatchCommitReceipt, MemoryError>;

    /// 经鉴权的 Persona 管理内容变更按 operation_id 幂等提交，不依赖对话 Turn。
    fn apply_management_content_mutation(
        &self,
        mutation: &MemoryManagementContentMutation,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryMutationReceipt, MemoryError>;

    /// 重要程度调整只更新逻辑 entry，不得创建或改写内容 revision。
    fn adjust_importance(
        &self,
        adjustment: &MemoryImportanceAdjustment,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError>;

    fn delete_confirmed(
        &self,
        request: &ConfirmedMemoryDeleteRequest,
        authority: &dyn MemoryDeletionAuthority,
    ) -> Result<MemoryDeleteReceipt, MemoryError>;
}

fn validate_deletion_subjects(
    subjects: &BTreeSet<MemoryDeletionSubject>,
) -> Result<(), MemoryError> {
    if subjects.is_empty() || subjects.len() > MAX_MEMORY_DELETION_SUBJECTS {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    for subject in subjects {
        subject.validate()?;
    }
    Ok(())
}

fn validate_deletion_field(value: &str) -> Result<(), MemoryError> {
    require_non_empty(value)?;
    if value.len() > MAX_MEMORY_DELETION_FIELD_BYTES {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    Ok(())
}

fn decode_derivation_digest(
    value: &str,
) -> Result<[u8; MemoryDerivationKey::DIGEST_LENGTH], MemoryError> {
    if value.len() != MemoryDerivationKey::DIGEST_LENGTH * 2
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }

    let mut digest = [0_u8; MemoryDerivationKey::DIGEST_LENGTH];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_digit(pair[0])?;
        let low = decode_hex_digit(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn decode_hex_digit(value: u8) -> Result<u8, MemoryError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(MemoryError::new(MemoryErrorCode::InvalidRequest)),
    }
}
