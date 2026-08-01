use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

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

/// 由当前直接用户消息确定性签发的来源资格。
///
/// 该类型不支持反序列化，也不暴露可自行填写的来源枚举。只有候选事实能在
/// 当前用户消息的规范化正文中逐字验证时才会构造成功；assistant、Tool、MCP、
/// 网页、文件、system 与 reasoning 单独提供的内容无法取得该资格。
#[derive(Clone, PartialEq, Eq)]
pub struct MemorySourceEligibility {
    scope: MemoryPersonaScope,
    source: MemorySourceEvidence,
    operation_id: String,
}

impl std::fmt::Debug for MemorySourceEligibility {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemorySourceEligibility")
            .field("scope", &self.scope)
            .field("source", &self.source)
            .field("evidence_binding", &"[确定性摘要]")
            .finish()
    }
}

impl MemorySourceEligibility {
    #[allow(clippy::too_many_arguments)]
    pub fn verify_direct_user_message(
        scope: MemoryPersonaScope,
        conversation_id: impl Into<String>,
        turn_id: impl Into<String>,
        call_id: impl Into<String>,
        direct_user_message: &str,
        candidate_content: &str,
    ) -> Result<Self, MemoryError> {
        let conversation_id = conversation_id.into();
        let turn_id = turn_id.into();
        let call_id = call_id.into();
        require_non_empty(&conversation_id)?;
        require_non_empty(&turn_id)?;
        require_opaque_token(&call_id)?;

        let normalized_source = normalize_source_evidence_text(direct_user_message)?;
        let normalized_candidate = normalize_source_evidence_text(candidate_content)?;
        let candidate_fact = canonical_direct_user_fact(&normalized_candidate);
        let whole_source_fact = canonical_direct_user_fact(&normalized_source);
        if candidate_fact.chars().count() < 2
            || has_indirect_source_marker(&normalized_source)
            || (whole_source_fact != candidate_fact
                && !normalized_source
                    .split(is_direct_user_clause_separator)
                    .map(canonical_direct_user_fact)
                    .any(|source_fact| source_fact == candidate_fact))
        {
            return Err(MemoryError::new(MemoryErrorCode::SourceIneligible));
        }

        let mut digest = Sha256::new();
        update_length_prefixed(&mut digest, b"muse-memory-source-evidence/v1");
        for field in [
            scope.persona_id(),
            conversation_id.as_str(),
            turn_id.as_str(),
            call_id.as_str(),
            normalized_source.as_str(),
            normalized_candidate.as_str(),
        ] {
            update_length_prefixed(&mut digest, field.as_bytes());
        }
        let operation_id = format!("memory-source-{}", encode_hex(&digest.finalize()));
        Ok(Self {
            scope,
            source: MemorySourceEvidence::ConversationTurn {
                conversation_id,
                turn_id,
                kind: MemorySourceKind::DirectUserMessage,
            },
            operation_id,
        })
    }
}

/// 来源、时间和幂等操作标识只能由已验证的运行时证据绑定。
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
    pub fn new(
        eligibility: MemorySourceEligibility,
        recorded_at: impl Into<String>,
        valid_from: impl Into<String>,
        freshness_at: impl Into<String>,
    ) -> Result<Self, MemoryError> {
        let recorded_at = recorded_at.into();
        let valid_from = valid_from.into();
        let freshness_at = freshness_at.into();
        for value in [&recorded_at, &valid_from, &freshness_at] {
            require_rfc3339(value)?;
        }
        Ok(Self {
            scope: eligibility.scope,
            source: eligibility.source,
            operation_id: eligibility.operation_id,
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

fn normalize_source_evidence_text(value: &str) -> Result<String, MemoryError> {
    require_non_empty(value).map_err(|_| MemoryError::new(MemoryErrorCode::SourceIneligible))?;
    let mut normalized = String::new();
    for character in value.nfkc().flat_map(char::to_lowercase) {
        if is_ignored_format_character(character) {
            continue;
        }
        if matches!(character, '\n' | '\r') {
            normalized.push('。');
            continue;
        }
        if character == '\t' {
            continue;
        }
        if character.is_control() {
            return Err(MemoryError::new(MemoryErrorCode::SourceIneligible));
        }
        let character = fold_common_homoglyph(character);
        if !character.is_whitespace() {
            normalized.push(character);
        }
    }
    if normalized.is_empty() {
        return Err(MemoryError::new(MemoryErrorCode::SourceIneligible));
    }
    Ok(normalized)
}

fn strip_direct_user_subject(value: &str) -> &str {
    const SUBJECTS: [&str; 5] = ["用户", "本人", "我自己", "我", "俺"];
    SUBJECTS
        .iter()
        .find_map(|subject| value.strip_prefix(subject))
        .unwrap_or(value)
}

fn canonical_direct_user_fact(value: &str) -> &str {
    const COMMAND_PREFIXES: [&str; 10] = [
        "请帮我记住",
        "请你记住",
        "请记住",
        "帮我记住",
        "记住",
        "另外",
        "还有",
        "并且",
        "而且",
        "也",
    ];
    let mut fact = value.trim_matches(is_direct_user_clause_separator);
    loop {
        let stripped = COMMAND_PREFIXES
            .iter()
            .find_map(|prefix| fact.strip_prefix(prefix))
            .unwrap_or(fact);
        let stripped = strip_direct_user_subject(stripped);
        if stripped == fact {
            break;
        }
        fact = stripped;
    }
    fact.trim_matches(is_direct_user_clause_separator)
}

fn is_direct_user_clause_separator(character: char) -> bool {
    matches!(
        character,
        '。' | '，' | ',' | '、' | '；' | ';' | '！' | '!' | '？' | '?'
    )
}

fn has_indirect_source_marker(value: &str) -> bool {
    const MARKERS: [&str; 20] = [
        "assistant说",
        "assistant声称",
        "tool说",
        "tool返回",
        "工具说",
        "工具返回",
        "工具结果",
        "mcp说",
        "mcp返回",
        "mcp结果",
        "网页说",
        "网页写着",
        "网页内容",
        "文件说",
        "文件写着",
        "文件内容",
        "资料写着",
        "文档写着",
        "system指令",
        "reasoning推测",
    ];
    MARKERS.iter().any(|marker| value.contains(marker))
}

fn is_ignored_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

fn fold_common_homoglyph(character: char) -> char {
    match character {
        'а' | 'α' => 'a',
        'в' | 'β' => 'b',
        'с' | 'ϲ' => 'c',
        'е' | 'ε' => 'e',
        'һ' | 'η' => 'h',
        'і' | 'ι' => 'i',
        'ј' => 'j',
        'κ' => 'k',
        'м' | 'μ' => 'm',
        'ո' => 'n',
        'о' | 'ο' => 'o',
        'р' | 'ρ' => 'p',
        'ѕ' => 's',
        'т' | 'τ' => 't',
        'х' | 'χ' => 'x',
        'у' | 'υ' => 'y',
        _ => character,
    }
}

fn update_length_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
    }
    encoded
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
#[derive(Clone, PartialEq, Eq)]
pub struct MemoryStagedMutation {
    params: MemoryMutateParams,
    binding: MemoryRuntimeBinding,
    assigned_memory_id: MemoryId,
    assigned_revision_id: MemoryRevisionId,
    staging_policy_version: String,
}

impl std::fmt::Debug for MemoryStagedMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryStagedMutation")
            .field("operation", &self.params.operation())
            .field("assigned_memory_id", &self.assigned_memory_id)
            .field("assigned_revision_id", &self.assigned_revision_id)
            .field("candidate_body", &"[已去敏]")
            .finish()
    }
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
        if permit.policy_version() != self.staging_policy_version {
            return Err(MemoryError::new(MemoryErrorCode::SensitivityUnavailable));
        }

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
#[derive(Clone, PartialEq, Eq)]
pub struct MemoryCommitEnvelope {
    idempotency_key: String,
    scope: MemoryPersonaScope,
    conversation_id: String,
    turn_id: String,
    committed_at: String,
    mutations: Vec<MemoryStagedMutation>,
}

impl std::fmt::Debug for MemoryCommitEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryCommitEnvelope")
            .field("idempotency_key", &self.idempotency_key)
            .field("scope", &self.scope)
            .field("conversation_id", &self.conversation_id)
            .field("turn_id", &self.turn_id)
            .field("mutation_count", &self.mutations.len())
            .field("candidate_bodies", &"[已去敏]")
            .finish()
    }
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
        approval_id: String,
        call_id: String,
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
                approval_id,
                call_id,
            } => {
                require_non_empty(conversation_id)?;
                require_non_empty(turn_id)?;
                require_opaque_token(approval_id)?;
                require_opaque_token(call_id)
            }
            Self::PersonaManagement { action_id } => require_opaque_token(action_id),
        }
    }
}

/// 已完成的专用用户确认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDeleteConfirmation {
    confirmation_id: String,
    persona_id: String,
    target_digest: String,
    confirmed_at: String,
    expires_at: String,
    source: MemoryDeleteConfirmationSource,
}

impl MemoryDeleteConfirmation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        confirmation_id: impl Into<String>,
        scope: &MemoryPersonaScope,
        params: &MemoryDeleteParams,
        confirmed_at: impl Into<String>,
        expires_at: impl Into<String>,
        source: MemoryDeleteConfirmationSource,
    ) -> Result<Self, MemoryError> {
        let confirmation_id = confirmation_id.into();
        let confirmed_at = confirmed_at.into();
        let expires_at = expires_at.into();
        require_opaque_token(&confirmation_id)?;
        require_rfc3339(&confirmed_at)?;
        require_rfc3339(&expires_at)?;
        params.validate()?;
        source.validate()?;
        let confirmed = chrono::DateTime::parse_from_rfc3339(&confirmed_at)
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?;
        let expires = chrono::DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?;
        if expires <= confirmed {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        match &source {
            MemoryDeleteConfirmationSource::ConversationTurn { approval_id, .. }
                if approval_id != &confirmation_id =>
            {
                return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
            }
            MemoryDeleteConfirmationSource::PersonaManagement { action_id }
                if action_id != &confirmation_id =>
            {
                return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
            }
            _ => {}
        }
        Ok(Self {
            confirmation_id,
            persona_id: scope.persona_id().to_string(),
            target_digest: normalized_delete_target_digest(scope, params)?,
            confirmed_at,
            expires_at,
            source,
        })
    }

    pub fn confirmation_id(&self) -> &str {
        &self.confirmation_id
    }

    pub fn confirmed_at(&self) -> &str {
        &self.confirmed_at
    }

    pub fn persona_id(&self) -> &str {
        &self.persona_id
    }

    pub fn target_digest(&self) -> &str {
        &self.target_digest
    }

    pub fn expires_at(&self) -> &str {
        &self.expires_at
    }

    pub fn source(&self) -> &MemoryDeleteConfirmationSource {
        &self.source
    }

    pub(crate) fn intent_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        update_length_prefixed(&mut digest, b"muse-memory-delete-confirmation/v1");
        for field in [
            self.confirmation_id.as_str(),
            self.persona_id.as_str(),
            self.target_digest.as_str(),
            self.confirmed_at.as_str(),
            self.expires_at.as_str(),
        ] {
            update_length_prefixed(&mut digest, field.as_bytes());
        }
        match &self.source {
            MemoryDeleteConfirmationSource::ConversationTurn {
                conversation_id,
                turn_id,
                approval_id,
                call_id,
            } => {
                update_length_prefixed(&mut digest, b"conversation_turn");
                for field in [conversation_id, turn_id, approval_id, call_id] {
                    update_length_prefixed(&mut digest, field.as_bytes());
                }
            }
            MemoryDeleteConfirmationSource::PersonaManagement { action_id } => {
                update_length_prefixed(&mut digest, b"persona_management");
                update_length_prefixed(&mut digest, action_id.as_bytes());
            }
        }
        digest.finalize().into()
    }

    fn validate_bound_request(
        &self,
        params: &MemoryDeleteParams,
        scope: &MemoryPersonaScope,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), MemoryError> {
        if self.persona_id != scope.persona_id()
            || self.target_digest != normalized_delete_target_digest(scope, params)?
        {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        let expires_at = chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map_err(|_| MemoryError::new(MemoryErrorCode::InvalidRequest))?
            .with_timezone(&chrono::Utc);
        if now >= expires_at {
            return Err(MemoryError::new(
                MemoryErrorCode::DeleteConfirmationRequired,
            ));
        }
        Ok(())
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
        confirmation.validate_bound_request(&params, &scope, chrono::Utc::now())?;
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

    pub(crate) fn validate_for_repository(&self) -> Result<(), MemoryError> {
        self.params.validate()?;
        self.confirmation
            .validate_bound_request(&self.params, &self.scope, chrono::Utc::now())
    }
}

fn normalized_delete_target_digest(
    scope: &MemoryPersonaScope,
    params: &MemoryDeleteParams,
) -> Result<String, MemoryError> {
    let persona = normalize_delete_target_component(scope.persona_id())?;
    let (kind, target) = match params {
        MemoryDeleteParams::Memory { memory_id } => (
            "memory",
            normalize_delete_target_component(memory_id.0.as_str())?,
        ),
        MemoryDeleteParams::PersonaAll => ("persona_all", "*".to_string()),
    };
    let mut digest = Sha256::new();
    update_length_prefixed(&mut digest, b"muse-memory-delete-target/v1");
    for field in [persona.as_str(), kind, target.as_str()] {
        update_length_prefixed(&mut digest, field.as_bytes());
    }
    Ok(encode_hex(&digest.finalize()))
}

fn normalize_delete_target_component(value: &str) -> Result<String, MemoryError> {
    require_non_empty(value)?;
    let mut normalized = String::new();
    for character in value.nfkc().flat_map(char::to_lowercase) {
        if is_ignored_format_character(character) {
            continue;
        }
        if character.is_control() {
            return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
        }
        let character = fold_common_homoglyph(character);
        if character.is_alphanumeric() {
            normalized.push(character);
        }
    }
    if normalized.is_empty() {
        return Err(MemoryError::new(MemoryErrorCode::InvalidRequest));
    }
    Ok(normalized)
}
