use serde::{Deserialize, Serialize};

use super::error::{require_non_empty, require_opaque_token, require_rfc3339};
use super::ports::MemoryPersistencePermit;
use super::{
    MemoryCategory, MemoryChangeType, MemoryEntry, MemoryEntryState, MemoryError, MemoryErrorCode,
    MemoryId, MemoryImportance, MemoryPersonaScope, MemoryRecord, MemoryRevision, MemoryRevisionId,
    MemoryRevisionState, MemorySafetyStage, MemorySensitivityPolicy, MemorySensitivityRequest,
    MemorySourceEvidence,
};

/// Persona 管理操作完成鉴权后由 runtime 创建的能力对象。
///
/// 该类型没有 serde 实现且字段私有，模型参数不能携带或伪造管理授权。
#[derive(Debug, PartialEq, Eq)]
pub struct MemoryManagementAuthorization {
    scope: MemoryPersonaScope,
    action_id: String,
    authorized_at: String,
}

impl MemoryManagementAuthorization {
    pub fn from_runtime(
        scope: MemoryPersonaScope,
        action_id: impl Into<String>,
        authorized_at: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let action_id = action_id.into();
        let authorized_at = authorized_at.into();
        require_opaque_token(&action_id)?;
        require_rfc3339(&authorized_at)?;
        Ok(Self {
            scope,
            action_id,
            authorized_at,
        })
    }
}

/// 不依赖 conversation/turn 的经鉴权 Persona 管理来源与时间绑定。
#[derive(Debug, PartialEq, Eq)]
pub struct MemoryManagementBinding {
    scope: MemoryPersonaScope,
    source: MemorySourceEvidence,
    operation_id: String,
    recorded_at: String,
    valid_from: String,
    freshness_at: String,
}

impl MemoryManagementBinding {
    pub fn bind(
        authorization: MemoryManagementAuthorization,
        operation_id: impl Into<String>,
        recorded_at: impl Into<String>,
        valid_from: impl Into<String>,
        freshness_at: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let operation_id = operation_id.into();
        let recorded_at = recorded_at.into();
        let valid_from = valid_from.into();
        let freshness_at = freshness_at.into();
        require_opaque_token(&operation_id)?;
        for value in [&recorded_at, &valid_from, &freshness_at] {
            require_rfc3339(value)?;
        }
        Ok(Self {
            scope: authorization.scope,
            source: MemorySourceEvidence::PersonaManagement {
                action_id: authorization.action_id,
                authorized_at: authorization.authorized_at,
            },
            operation_id,
            recorded_at,
            valid_from,
            freshness_at,
        })
    }

    pub fn scope(&self) -> &MemoryPersonaScope {
        &self.scope
    }

    pub fn source(&self) -> &MemorySourceEvidence {
        &self.source
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn recorded_at(&self) -> &str {
        &self.recorded_at
    }

    pub fn valid_from(&self) -> &str {
        &self.valid_from
    }

    pub fn freshness_at(&self) -> &str {
        &self.freshness_at
    }
}

/// Persona 管理页可提交的内容变更；该 runtime DTO 不向模型反序列化。
///
/// 管理页只需要手工新增和纠正。重要程度调整由独立类型承载，纠正内容时会保留
/// 当前 entry 的重要程度。
#[derive(Clone, PartialEq, Eq)]
pub enum MemoryManagementContentParams {
    Create {
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
        event_time: Option<String>,
        change_reason: String,
    },
}

impl std::fmt::Debug for MemoryManagementContentParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryManagementContentParams")
            .field("operation", &self.operation())
            .field("category", &self.category())
            .field("importance", &self.importance())
            .field("has_event_time", &self.event_time().is_some())
            .field("content", &"[已去敏]")
            .field("change_reason", &"[已去敏]")
            .finish()
    }
}

impl MemoryManagementContentParams {
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

    pub const fn operation(&self) -> MemoryChangeType {
        match self {
            Self::Create { .. } => MemoryChangeType::Create,
            Self::Correct { .. } => MemoryChangeType::Correct,
        }
    }

    pub const fn category(&self) -> MemoryCategory {
        match self {
            Self::Create { category, .. } | Self::Correct { category, .. } => *category,
        }
    }

    pub const fn importance(&self) -> Option<MemoryImportance> {
        match self {
            Self::Create { importance, .. } => Some(*importance),
            Self::Correct { .. } => None,
        }
    }

    pub fn content(&self) -> &str {
        match self {
            Self::Create { content, .. } | Self::Correct { content, .. } => content,
        }
    }

    pub fn event_time(&self) -> Option<&str> {
        match self {
            Self::Create { event_time, .. } | Self::Correct { event_time, .. } => {
                event_time.as_deref()
            }
        }
    }

    pub fn change_reason(&self) -> &str {
        match self {
            Self::Create { change_reason, .. } | Self::Correct { change_reason, .. } => {
                change_reason
            }
        }
    }

    pub fn memory_id(&self) -> Option<&MemoryId> {
        match self {
            Self::Create { .. } => None,
            Self::Correct { memory_id, .. } => Some(memory_id),
        }
    }

    pub fn expected_revision_id(&self) -> Option<&MemoryRevisionId> {
        match self {
            Self::Create { .. } => None,
            Self::Correct {
                expected_revision_id,
                ..
            } => Some(expected_revision_id),
        }
    }
}

/// 已绑定鉴权来源、时间与 runtime 分配标识的管理内容变更。
#[derive(Debug, PartialEq, Eq)]
pub struct MemoryManagementContentMutation {
    params: MemoryManagementContentParams,
    binding: MemoryManagementBinding,
    assigned_memory_id: MemoryId,
    assigned_revision_id: MemoryRevisionId,
    staging_policy_version: String,
}

impl MemoryManagementContentMutation {
    pub fn bind(
        params: MemoryManagementContentParams,
        binding: MemoryManagementBinding,
        assigned_memory_id: MemoryId,
        assigned_revision_id: MemoryRevisionId,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<Self, MemoryError> {
        params.validate()?;
        require_non_empty(&assigned_memory_id.0)?;
        require_non_empty(&assigned_revision_id.0)?;
        if let Some(memory_id) = params.memory_id()
            && memory_id != &assigned_memory_id
        {
            return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
        }

        let request = sensitivity_request_for_management_mutation(
            MemorySafetyStage::TurnStaging,
            &params,
            &binding,
            &assigned_memory_id,
            &assigned_revision_id,
        );
        let permit = sensitivity.assess(request).into_staging_permit(request)?;
        permit.validate(request)?;

        Ok(Self {
            params,
            binding,
            assigned_memory_id,
            assigned_revision_id,
            staging_policy_version: permit.policy_version().to_string(),
        })
    }

    pub fn params(&self) -> &MemoryManagementContentParams {
        &self.params
    }

    pub fn binding(&self) -> &MemoryManagementBinding {
        &self.binding
    }

    pub fn assigned_memory_id(&self) -> &MemoryId {
        &self.assigned_memory_id
    }

    pub fn assigned_revision_id(&self) -> &MemoryRevisionId {
        &self.assigned_revision_id
    }

    pub fn staging_policy_version(&self) -> &str {
        &self.staging_policy_version
    }

    pub fn sensitivity_request(&self, stage: MemorySafetyStage) -> MemorySensitivityRequest<'_> {
        sensitivity_request_for_management_mutation(
            stage,
            &self.params,
            &self.binding,
            &self.assigned_memory_id,
            &self.assigned_revision_id,
        )
    }

    /// Repository 在持久化前调用；敏感许可在方法内签发并立即绑定消费。
    pub fn transition(
        &self,
        current: Option<&MemoryRecord>,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<super::MemoryMutationTransition, MemoryError> {
        let request = self.sensitivity_request(MemorySafetyStage::RepositoryCommit);
        let permit = sensitivity
            .assess(request)
            .into_repository_permit(request)?;
        permit.validate(request)?;
        if permit.policy_version() != self.staging_policy_version {
            return Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable));
        }

        match &self.params {
            MemoryManagementContentParams::Create { importance, .. } => {
                if current.is_some() {
                    return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
                }
                Ok(super::MemoryMutationTransition {
                    entry: self.new_entry(self.binding.recorded_at.clone(), *importance),
                    previous_revision: None,
                    new_revision: self.new_revision(&permit),
                })
            }
            MemoryManagementContentParams::Correct {
                expected_revision_id,
                ..
            } => self.correct_existing(current, expected_revision_id, &permit),
        }
    }

    fn correct_existing(
        &self,
        current: Option<&MemoryRecord>,
        expected_revision_id: &MemoryRevisionId,
        permit: &MemoryPersistencePermit,
    ) -> Result<super::MemoryMutationTransition, MemoryError> {
        let current = current.ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if current.entry.persona_id != self.binding.scope.persona_id()
            || current.entry.memory_id != self.assigned_memory_id
        {
            return Err(MemoryError::new(MemoryErrorCode::PersonaScopeMismatch));
        }
        if current.entry.state != MemoryEntryState::Active
            || current.current_revision.state != MemoryRevisionState::Current
            || current.entry.current_revision_id != current.current_revision.revision_id
            || current.current_revision.memory_id != current.entry.memory_id
        {
            return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
        }
        if &current.current_revision.revision_id != expected_revision_id {
            return Err(MemoryError::new(MemoryErrorCode::RevisionConflict));
        }
        if &self.assigned_revision_id == expected_revision_id {
            return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
        }

        let mut previous_revision = current.current_revision.clone();
        previous_revision.state = MemoryRevisionState::Corrected;
        previous_revision.valid_to = Some(self.binding.valid_from.clone());
        Ok(super::MemoryMutationTransition {
            entry: self.new_entry(current.entry.created_at.clone(), current.entry.importance),
            previous_revision: Some(previous_revision),
            new_revision: self.new_revision(permit),
        })
    }

    fn new_entry(&self, created_at: String, importance: MemoryImportance) -> MemoryEntry {
        MemoryEntry {
            memory_id: self.assigned_memory_id.clone(),
            persona_id: self.binding.scope.persona_id().to_string(),
            category: self.params.category(),
            current_revision_id: self.assigned_revision_id.clone(),
            importance,
            freshness_at: self.binding.freshness_at.clone(),
            created_at,
            state: MemoryEntryState::Active,
        }
    }

    fn new_revision(&self, permit: &MemoryPersistencePermit) -> MemoryRevision {
        MemoryRevision {
            revision_id: self.assigned_revision_id.clone(),
            memory_id: self.assigned_memory_id.clone(),
            content: self.params.content().trim().to_string(),
            event_time: self.params.event_time().map(str::to_string),
            recorded_at: self.binding.recorded_at.clone(),
            valid_from: self.binding.valid_from.clone(),
            valid_to: None,
            change_type: self.params.operation(),
            change_reason: self.params.change_reason().trim().to_string(),
            source: self.binding.source.clone(),
            safety_policy_version: permit.policy_version().to_string(),
            state: MemoryRevisionState::Current,
        }
    }
}

fn sensitivity_request_for_management_mutation<'a>(
    stage: MemorySafetyStage,
    params: &'a MemoryManagementContentParams,
    binding: &'a MemoryManagementBinding,
    assigned_memory_id: &'a MemoryId,
    assigned_revision_id: &'a MemoryRevisionId,
) -> MemorySensitivityRequest<'a> {
    MemorySensitivityRequest {
        stage,
        operation_id: binding.operation_id(),
        operation: params.operation(),
        memory_id: params.memory_id(),
        expected_revision_id: params.expected_revision_id(),
        assigned_memory_id,
        assigned_revision_id,
        category: params.category(),
        importance: params.importance(),
        content: params.content(),
        change_reason: params.change_reason(),
        event_time: params.event_time(),
        source: binding.source(),
    }
}

/// 经鉴权的独立重要程度调整，不携带 content、category 或 revision 写入字段。
#[derive(Debug, PartialEq, Eq)]
pub struct MemoryImportanceAdjustment {
    binding: MemoryManagementBinding,
    memory_id: MemoryId,
    expected_revision_id: MemoryRevisionId,
    expected_importance: MemoryImportance,
    importance: MemoryImportance,
}

impl MemoryImportanceAdjustment {
    pub fn bind(
        binding: MemoryManagementBinding,
        memory_id: MemoryId,
        expected_revision_id: MemoryRevisionId,
        expected_importance: MemoryImportance,
        importance: MemoryImportance,
    ) -> Result<Self, MemoryError> {
        require_non_empty(&memory_id.0)?;
        require_non_empty(&expected_revision_id.0)?;
        if expected_importance == importance {
            return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
        }
        Ok(Self {
            binding,
            memory_id,
            expected_revision_id,
            expected_importance,
            importance,
        })
    }

    pub fn binding(&self) -> &MemoryManagementBinding {
        &self.binding
    }

    pub fn memory_id(&self) -> &MemoryId {
        &self.memory_id
    }

    pub fn expected_revision_id(&self) -> &MemoryRevisionId {
        &self.expected_revision_id
    }

    pub const fn expected_importance(&self) -> MemoryImportance {
        self.expected_importance
    }

    pub const fn importance(&self) -> MemoryImportance {
        self.importance
    }

    /// 仅形成 entry 更新；返回值不包含也不创建 MemoryRevision。
    pub fn apply_to(&self, current: &MemoryRecord) -> Result<MemoryEntry, MemoryError> {
        if current.entry.persona_id != self.binding.scope.persona_id()
            || current.entry.memory_id != self.memory_id
        {
            return Err(MemoryError::new(MemoryErrorCode::PersonaScopeMismatch));
        }
        if current.entry.state != MemoryEntryState::Active
            || current.current_revision.state != MemoryRevisionState::Current
            || current.entry.current_revision_id != current.current_revision.revision_id
            || current.current_revision.memory_id != current.entry.memory_id
        {
            return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
        }
        if current.current_revision.revision_id != self.expected_revision_id {
            return Err(MemoryError::new(MemoryErrorCode::RevisionConflict));
        }
        if current.entry.importance != self.expected_importance {
            return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
        }
        let mut adjusted = current.entry.clone();
        adjusted.importance = self.importance;
        adjusted.freshness_at = self.binding.freshness_at.clone();
        Ok(adjusted)
    }
}

/// 重要程度调整收据有意不包含 revision_id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryImportanceAdjustmentReceipt {
    pub operation_id: String,
    pub memory_id: MemoryId,
    pub previous_importance: MemoryImportance,
    pub importance: MemoryImportance,
    pub durable_at: String,
}
