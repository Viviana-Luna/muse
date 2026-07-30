use std::collections::BTreeSet;

use super::error::{require_non_empty, require_opaque_token, require_rfc3339};
use super::ports::MemoryPersistencePermit;
use super::{
    MemoryDeleteParams, MemoryEntry, MemoryEntryState, MemoryError, MemoryErrorCode, MemoryId,
    MemoryMutateParams, MemoryMutationReceipt, MemoryMutationReceiptState, MemoryQueryParams,
    MemoryRecord, MemoryRevision, MemoryRevisionId, MemoryRevisionState, MemorySafetyStage,
    MemorySensitivityPolicy, MemorySensitivityRequest, MemorySourceEvidence, MemorySourceKind,
};

/// 运行时绑定的 Persona scope；该类型不支持反序列化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPersonaScope {
    persona_id: String,
}

impl MemoryPersonaScope {
    pub fn new(persona_id: impl Into<String>) -> Result<Self, MemoryError> {
        let persona_id = persona_id.into();
        require_non_empty(&persona_id)?;
        Ok(Self { persona_id })
    }

    pub fn persona_id(&self) -> &str {
        &self.persona_id
    }
}

/// 来源、时间和幂等操作标识只能由冻结 Turn 运行时绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRuntimeBinding {
    scope: MemoryPersonaScope,
    source: MemorySourceEvidence,
    operation_id: String,
    recorded_at: String,
    valid_from: String,
    freshness_at: String,
}

impl MemoryRuntimeBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope: MemoryPersonaScope,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
        operation_id: impl Into<String>,
        source_kind: MemorySourceKind,
        recorded_at: impl Into<String>,
        valid_from: impl Into<String>,
        freshness_at: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let conversation_id = conversation_id.into();
        let turn_id = turn_id.into();
        let operation_id = operation_id.into();
        let recorded_at = recorded_at.into();
        let valid_from = valid_from.into();
        let freshness_at = freshness_at.into();
        require_non_empty(&conversation_id)?;
        require_non_empty(&turn_id)?;
        require_opaque_token(&operation_id)?;
        for value in [&recorded_at, &valid_from, &freshness_at] {
            require_rfc3339(value)?;
        }
        Ok(Self {
            scope,
            source: MemorySourceEvidence::ConversationTurn {
                conversation_id,
                turn_id,
                kind: source_kind,
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

/// 已绑定 Persona scope 的查询请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRetrievalRequest {
    pub params: MemoryQueryParams,
    pub scope: MemoryPersonaScope,
}

impl MemoryRetrievalRequest {
    pub fn bind(params: MemoryQueryParams, scope: MemoryPersonaScope) -> Result<Self, MemoryError> {
        params.validate()?;
        Ok(Self { params, scope })
    }
}

/// 已通过第一层敏感门并暂存在当前 Turn 内存中的变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryStagedMutation {
    params: MemoryMutateParams,
    binding: MemoryRuntimeBinding,
    assigned_memory_id: MemoryId,
    assigned_revision_id: MemoryRevisionId,
    staging_policy_version: String,
}

impl MemoryStagedMutation {
    pub fn stage(
        params: MemoryMutateParams,
        binding: MemoryRuntimeBinding,
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

        let request = sensitivity_request_for_mutation(
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

    pub fn params(&self) -> &MemoryMutateParams {
        &self.params
    }

    pub fn binding(&self) -> &MemoryRuntimeBinding {
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
        sensitivity_request_for_mutation(
            stage,
            &self.params,
            &self.binding,
            &self.assigned_memory_id,
            &self.assigned_revision_id,
        )
    }

    pub fn staged_receipt(&self) -> MemoryMutationReceipt {
        MemoryMutationReceipt {
            operation: self.params.operation(),
            memory_id: self.assigned_memory_id.clone(),
            revision_id: self.assigned_revision_id.clone(),
            state: MemoryMutationReceiptState::Staged,
        }
    }

    /// 根据当前一致记录生成一次原子 transition。
    pub fn transition(
        &self,
        current: Option<&MemoryRecord>,
        sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryMutationTransition, MemoryError> {
        let request = self.sensitivity_request(MemorySafetyStage::RepositoryCommit);
        let permit = sensitivity
            .assess(request)
            .into_repository_permit(request)?;
        permit.validate(request)?;

        match &self.params {
            MemoryMutateParams::Create { .. } => {
                if current.is_some() {
                    return Err(MemoryError::new(MemoryErrorCode::InvalidStateTransition));
                }
                Ok(MemoryMutationTransition {
                    entry: self.new_entry(),
                    previous_revision: None,
                    new_revision: self.new_revision(&permit),
                })
            }
            MemoryMutateParams::Update {
                expected_revision_id,
                ..
            } => self.transition_existing(
                current,
                expected_revision_id,
                MemoryRevisionState::Superseded,
                &permit,
            ),
            MemoryMutateParams::Correct {
                expected_revision_id,
                ..
            } => self.transition_existing(
                current,
                expected_revision_id,
                MemoryRevisionState::Corrected,
                &permit,
            ),
        }
    }

    fn transition_existing(
        &self,
        current: Option<&MemoryRecord>,
        expected_revision_id: &MemoryRevisionId,
        previous_state: MemoryRevisionState,
        permit: &MemoryPersistencePermit,
    ) -> Result<MemoryMutationTransition, MemoryError> {
        let current = current.ok_or_else(|| MemoryError::new(MemoryErrorCode::MemoryNotFound))?;
        if current.entry.persona_id != self.binding.scope.persona_id
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
        previous_revision.state = previous_state;
        previous_revision.valid_to = Some(self.binding.valid_from.clone());
        Ok(MemoryMutationTransition {
            entry: self.new_entry_with_created_at(current.entry.created_at.clone()),
            previous_revision: Some(previous_revision),
            new_revision: self.new_revision(permit),
        })
    }

    fn new_entry(&self) -> MemoryEntry {
        self.new_entry_with_created_at(self.binding.recorded_at.clone())
    }

    fn new_entry_with_created_at(&self, created_at: String) -> MemoryEntry {
        MemoryEntry {
            memory_id: self.assigned_memory_id.clone(),
            persona_id: self.binding.scope.persona_id.clone(),
            category: self.params.category(),
            current_revision_id: self.assigned_revision_id.clone(),
            importance: self.params.importance(),
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

/// Repository 应在同一原子提交中应用的完整状态变化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryMutationTransition {
    pub entry: MemoryEntry,
    pub previous_revision: Option<MemoryRevision>,
    pub new_revision: MemoryRevision,
}

/// 同一 committed Turn 的原子、幂等提交封套。
///
/// 封套只能接收 `MemoryStagedMutation`，因此未通过第一层敏感门的提议无法进入
/// Repository 批量提交端口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCommitEnvelope {
    idempotency_key: String,
    scope: MemoryPersonaScope,
    conversation_id: String,
    turn_id: String,
    committed_at: String,
    mutations: Vec<MemoryStagedMutation>,
}

impl MemoryCommitEnvelope {
    pub fn new(
        idempotency_key: impl Into<String>,
        scope: MemoryPersonaScope,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
        committed_at: impl Into<String>,
        mutations: Vec<MemoryStagedMutation>,
    ) -> Result<Self, MemoryError> {
        let idempotency_key = idempotency_key.into();
        let conversation_id = conversation_id.into();
        let turn_id = turn_id.into();
        let committed_at = committed_at.into();
        require_opaque_token(&idempotency_key)?;
        require_non_empty(&conversation_id)?;
        require_non_empty(&turn_id)?;
        require_rfc3339(&committed_at)?;
        if mutations.is_empty() {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }

        let mut operation_ids = BTreeSet::new();
        let mut revision_ids = BTreeSet::new();
        for mutation in &mutations {
            let binding = mutation.binding();
            if binding.scope() != &scope
                || binding.source().conversation_id() != Some(conversation_id.as_str())
                || binding.source().turn_id() != Some(turn_id.as_str())
            {
                return Err(MemoryError::new(MemoryErrorCode::SourceIneligible));
            }
            if !operation_ids.insert(binding.operation_id())
                || !revision_ids.insert(mutation.assigned_revision_id())
            {
                return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
            }
        }

        Ok(Self {
            idempotency_key,
            scope,
            conversation_id,
            turn_id,
            committed_at,
            mutations,
        })
    }

    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    pub fn scope(&self) -> &MemoryPersonaScope {
        &self.scope
    }

    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub fn committed_at(&self) -> &str {
        &self.committed_at
    }

    pub fn mutations(&self) -> &[MemoryStagedMutation] {
        &self.mutations
    }
}

fn sensitivity_request_for_mutation<'a>(
    stage: MemorySafetyStage,
    params: &'a MemoryMutateParams,
    binding: &'a MemoryRuntimeBinding,
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
        importance: Some(params.importance()),
        content: params.content(),
        change_reason: params.change_reason(),
        event_time: params.event_time(),
        source: binding.source(),
    }
}

/// 用户确认来源；该运行时证据不支持模型反序列化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryDeleteConfirmationSource {
    ConversationTurn {
        conversation_id: String,
        turn_id: String,
    },
    PersonaManagement {
        action_id: String,
    },
}

impl MemoryDeleteConfirmationSource {
    fn validate(&self) -> Result<(), MemoryError> {
        match self {
            Self::ConversationTurn {
                conversation_id,
                turn_id,
            } => {
                require_non_empty(conversation_id)?;
                require_non_empty(turn_id)
            }
            Self::PersonaManagement { action_id } => require_opaque_token(action_id),
        }
    }
}

/// 已完成的专用用户确认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeleteConfirmation {
    confirmation_id: String,
    confirmed_at: String,
    source: MemoryDeleteConfirmationSource,
}

impl MemoryDeleteConfirmation {
    pub fn new(
        confirmation_id: impl Into<String>,
        confirmed_at: impl Into<String>,
        source: MemoryDeleteConfirmationSource,
    ) -> Result<Self, MemoryError> {
        let confirmation_id = confirmation_id.into();
        let confirmed_at = confirmed_at.into();
        require_opaque_token(&confirmation_id)?;
        require_rfc3339(&confirmed_at)?;
        source.validate()?;
        Ok(Self {
            confirmation_id,
            confirmed_at,
            source,
        })
    }

    pub fn confirmation_id(&self) -> &str {
        &self.confirmation_id
    }

    pub fn confirmed_at(&self) -> &str {
        &self.confirmed_at
    }

    pub fn source(&self) -> &MemoryDeleteConfirmationSource {
        &self.source
    }
}

/// Repository 唯一允许接受的删除请求，原始模型参数不能直接删除。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedMemoryDeleteRequest {
    params: MemoryDeleteParams,
    scope: MemoryPersonaScope,
    confirmation: MemoryDeleteConfirmation,
}

impl ConfirmedMemoryDeleteRequest {
    pub fn bind(
        params: MemoryDeleteParams,
        scope: MemoryPersonaScope,
        confirmation: MemoryDeleteConfirmation,
    ) -> Result<Self, MemoryError> {
        params.validate()?;
        Ok(Self {
            params,
            scope,
            confirmation,
        })
    }

    pub fn params(&self) -> &MemoryDeleteParams {
        &self.params
    }

    pub fn scope(&self) -> &MemoryPersonaScope {
        &self.scope
    }

    pub fn confirmation(&self) -> &MemoryDeleteConfirmation {
        &self.confirmation
    }
}
