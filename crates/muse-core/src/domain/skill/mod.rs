//! 运行底座技能边界。
//!
//! 技能是面向模型的可装载能力说明，不等同于角色、工具或 MCP 服务。这里先定义
//! 核心侧稳定数据结构，后续技能加载器、技能目录扫描和 MCP 技能构建器都应收口到
//! 这个边界之下。

use serde::{Deserialize, Serialize};

pub mod config;
pub mod store;

pub use config::{SkillConfigOverride, SkillPreferences, skill_document_path};
pub use store::{
    MAX_SKILL_DOCUMENT_BYTES, SkillCatalogDiagnostic, SkillCatalogSnapshot, SkillDraft,
    SkillRecord, SkillStore, SkillStoreError, SkillStoreErrorKind, SkillSummary,
    migrate_legacy_skill_enabled, validate_skill_name,
};

/// 技能来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Builtin,
    User,
    Mcp,
}

/// 暴露给运行底座的技能摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub enabled: bool,
}

impl SkillDef {
    /// 创建默认启用的技能摘要。
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        source: SkillSource,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            source,
            enabled: true,
        }
    }
}
