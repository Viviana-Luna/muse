//! 角色接口的请求与响应 DTO。

use muse_core::domain::persona::character::card::{
    PersonaCard, PersonaCardConflictStrategy, PersonaCardExportLevel,
};
use muse_core::domain::persona::visual::VisualPack;
use muse_core::domain::persona::{Persona, PersonaSummary};
use serde::{Deserialize, Serialize};

/// 角色创建或更新请求体。
#[derive(Deserialize)]
pub struct PersonaUpsertRequest {
    pub persona: Persona,
    #[serde(default)]
    pub visual_pack_patch: Option<PersonaVisualPackPatch>,
    /// 创建成功后在同一 transition gate 内激活角色并重置会话。
    #[serde(default)]
    pub activate_after_create: bool,
}

/// 角色编辑页提交的展示包补丁。
#[derive(Clone, Deserialize)]
pub struct PersonaVisualPackPatch {
    pub portrait_path: String,
    #[serde(default)]
    pub background_path: Option<String>,
    #[serde(default)]
    pub avatar_path: Option<String>,
    #[serde(default)]
    pub theme_color: Option<String>,
    #[serde(default)]
    pub theme_mode: Option<String>,
    #[serde(default)]
    pub portrait_frame: Option<String>,
    #[serde(default)]
    pub portrait_fit: Option<String>,
    #[serde(default)]
    pub portrait_position_x: Option<i32>,
    #[serde(default)]
    pub portrait_position_y: Option<i32>,
    #[serde(default)]
    pub portrait_scale: Option<u16>,
}

/// 角色卡片导出查询参数。
#[derive(Deserialize)]
pub struct PersonaCardExportQuery {
    #[serde(default)]
    pub level: PersonaCardExportLevel,
}

/// 角色卡片导入请求体。
#[derive(Deserialize)]
pub struct PersonaCardImportRequest {
    pub card: PersonaCard,
    #[serde(default)]
    pub conflict_strategy: PersonaCardConflictStrategy,
    #[serde(default)]
    pub activate_after_import: bool,
}

/// 角色列表响应体。
#[derive(Serialize)]
pub struct PersonaListResponse {
    pub personas: Vec<PersonaLibraryItem>,
    pub active_persona_id: Option<String>,
}

/// 角色库列表项，保留轻量摘要并附带已解析的图片预览事实。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersonaLibraryItem {
    #[serde(flatten)]
    pub persona: PersonaSummary,
    pub visual_preview: PersonaVisualPreview,
}

/// 角色库卡片可使用的图片路径；缺失保持为 `null`，不伪造首字头像。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersonaVisualPreview {
    pub avatar_path: Option<String>,
    pub portrait_path: Option<String>,
}

impl PersonaLibraryItem {
    pub fn from_summary(persona: PersonaSummary, visual_pack: Option<&VisualPack>) -> Self {
        let visual_preview = PersonaVisualPreview {
            avatar_path: visual_pack.and_then(|pack| normalized_visual_path(&pack.avatar_path)),
            portrait_path: visual_pack.and_then(|pack| normalized_visual_path(&pack.portrait_path)),
        };
        Self {
            persona,
            visual_preview,
        }
    }
}

fn normalized_visual_path(path: &str) -> Option<String> {
    let normalized = path.trim();
    (!normalized.is_empty()).then(|| normalized.to_string())
}

/// 当前激活角色响应体。
#[derive(Serialize)]
pub struct ActivePersonaResponse {
    pub active_persona: Option<Persona>,
    pub active_persona_id: Option<String>,
    pub visual_pack: Option<VisualPack>,
    pub state_revision: u64,
}

/// 角色详情响应体。
#[derive(Serialize)]
pub struct PersonaDetailResponse {
    pub persona: Persona,
    pub visual_pack: Option<VisualPack>,
    pub runtime_state: Option<muse_runtime::persona_state::EffectivePersonaState>,
}

#[derive(Serialize)]
pub struct PersonaDeletionImpactResponse {
    pub persona_id: String,
    pub associated_session_count: usize,
    pub workspace_state_exists: bool,
    /// 记忆服务未接线或计数不可用时为 `null`；前端必须保持确认按钮禁用。
    pub memory_count: Option<u64>,
}

/// 角色变更响应体。
#[derive(Serialize)]
pub struct PersonaMutationResponse {
    pub affected_persona: Persona,
    pub active_persona: Option<Persona>,
    pub active_persona_id: Option<String>,
    pub visual_pack: Option<VisualPack>,
    pub runtime_reset: bool,
    pub conversation_id: String,
    pub active_conversation_id: String,
    pub session_restored: bool,
    pub state_revision: u64,
}

/// 角色卡片导入响应体。
#[derive(Serialize)]
pub struct PersonaCardImportResponse {
    pub affected_persona: Persona,
    pub active_persona: Option<Persona>,
    pub active_persona_id: Option<String>,
    pub visual_pack: Option<VisualPack>,
    pub notices: Vec<String>,
    pub runtime_reset: bool,
    pub conversation_id: String,
    pub state_revision: u64,
}
