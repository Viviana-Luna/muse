use serde::{Deserialize, Serialize};

/// 记忆的封闭业务类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    UserFact,
    UserPreference,
    SharedExperience,
    Commitment,
    StoryState,
}

/// 记忆检索使用的封闭主题维度。
///
/// category 表达业务来源，facet 表达用户提问时最常用的检索入口；两者分开保存，
/// 避免把不断扩张的自然语言主题塞进 category。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFacet {
    Identity,
    Timezone,
    Location,
    Occupation,
    PreferenceFood,
    PreferenceDrink,
    PreferenceCommunication,
    PreferenceTool,
    PreferenceOther,
    Habit,
    Plan,
    SharedExperience,
    Commitment,
    StoryState,
    Other,
}

impl MemoryFacet {
    pub const fn default_for_category(category: MemoryCategory) -> Self {
        match category {
            MemoryCategory::UserFact => Self::Other,
            MemoryCategory::UserPreference => Self::PreferenceOther,
            MemoryCategory::SharedExperience => Self::SharedExperience,
            MemoryCategory::Commitment => Self::Commitment,
            MemoryCategory::StoryState => Self::StoryState,
        }
    }

    pub const fn is_compatible_with(self, category: MemoryCategory) -> bool {
        match category {
            MemoryCategory::UserFact => matches!(
                self,
                Self::Identity
                    | Self::Timezone
                    | Self::Location
                    | Self::Occupation
                    | Self::Habit
                    | Self::Plan
                    | Self::Other
            ),
            MemoryCategory::UserPreference => matches!(
                self,
                Self::PreferenceFood
                    | Self::PreferenceDrink
                    | Self::PreferenceCommunication
                    | Self::PreferenceTool
                    | Self::PreferenceOther
            ),
            MemoryCategory::SharedExperience => matches!(self, Self::SharedExperience),
            MemoryCategory::Commitment => matches!(self, Self::Commitment),
            MemoryCategory::StoryState => matches!(self, Self::StoryState),
        }
    }
}

/// 记忆的重要程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryImportance {
    Low,
    Normal,
    High,
}

/// 逻辑记忆状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEntryState {
    Active,
    Deleted,
}

impl MemoryEntryState {
    pub const fn is_model_readable(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// revision 在版本链中的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRevisionState {
    Current,
    Superseded,
    Corrected,
}

impl MemoryRevisionState {
    /// 被纠正的错误历史永远不能进入模型读取面。
    pub const fn is_model_readable(self) -> bool {
        matches!(self, Self::Current | Self::Superseded)
    }
}

/// revision 的变化语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryChangeType {
    Create,
    Update,
    Correct,
}

/// 允许作为记忆依据的来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySourceKind {
    DirectUserMessage,
    UserConfirmation,
    DeterministicLocalEvent,
}

/// 逻辑记忆标识。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryId(pub String);

/// 记忆 revision 标识。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryRevisionId(pub String);

/// 逻辑记忆。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntry {
    pub memory_id: MemoryId,
    pub persona_id: String,
    pub category: MemoryCategory,
    pub current_revision_id: MemoryRevisionId,
    pub importance: MemoryImportance,
    pub freshness_at: String,
    pub created_at: String,
    pub state: MemoryEntryState,
}

/// 一次由运行时绑定的可靠来源证据。
///
/// 对话来源与经鉴权的 Persona 管理操作使用互斥结构，避免为了兼容管理入口而
/// 伪造 conversation/turn。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemorySourceEvidence {
    ConversationTurn {
        conversation_id: String,
        turn_id: String,
        kind: MemorySourceKind,
    },
    PersonaManagement {
        action_id: String,
        authorized_at: String,
    },
}

impl MemorySourceEvidence {
    pub fn conversation_id(&self) -> Option<&str> {
        match self {
            Self::ConversationTurn {
                conversation_id, ..
            } => Some(conversation_id),
            Self::PersonaManagement { .. } => None,
        }
    }

    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Self::ConversationTurn { turn_id, .. } => Some(turn_id),
            Self::PersonaManagement { .. } => None,
        }
    }
}

/// 一条记忆 revision。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRevision {
    pub revision_id: MemoryRevisionId,
    pub memory_id: MemoryId,
    pub facet: MemoryFacet,
    pub keywords: Vec<String>,
    pub content: String,
    pub event_time: Option<String>,
    pub recorded_at: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub change_type: MemoryChangeType,
    pub change_reason: String,
    pub source: MemorySourceEvidence,
    pub safety_policy_version: String,
    pub state: MemoryRevisionState,
}

/// Repository 读取到的当前一致记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    pub entry: MemoryEntry,
    pub current_revision: MemoryRevision,
}
