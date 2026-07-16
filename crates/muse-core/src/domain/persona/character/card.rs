use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::store::PersonaStore;
use super::{Persona, PersonaValidationError};
use crate::domain::persona::visual::store::VisualPackStore;
use crate::domain::persona::visual::{VisualPack, VisualPackValidationError};

pub const PERSONA_CARD_SCHEMA_VERSION: &str = "muse-role-card/v1";
const LEGACY_PERSONA_CARD_SCHEMA_VERSION: &str = "agent-vp-persona-card/v1";

/// 角色卡片导出层级。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PersonaCardExportLevel {
    /// 仅导出角色主体配置。
    PersonaOnly,
    /// 导出角色配置，并附带展示包引用快照。
    #[default]
    WithVisualPackRef,
}

/// 角色卡片导入时的冲突处理策略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PersonaCardConflictStrategy {
    /// 遇到重复 id 时直接取消导入。
    Cancel,
    /// 遇到重复 id 时覆盖本地已有配置。
    Overwrite,
    /// 遇到重复 id 时自动生成一份新副本。
    #[default]
    Rename,
}

/// 可导入导出的角色卡片主体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaCard {
    #[serde(default = "default_persona_card_schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub exported_at: String,
    #[serde(default)]
    pub export_level: PersonaCardExportLevel,
    pub persona: Persona,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_pack: Option<VisualPack>,
}

/// 卡片导入准备结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaCardImportPlan {
    pub persona: Persona,
    pub visual_pack: Option<VisualPack>,
    pub notices: Vec<String>,
}

/// 角色卡片相关错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaCardError {
    UnsupportedSchemaVersion(String),
    Validation(PersonaValidationError),
    VisualPackValidation(VisualPackValidationError),
    Conflict(String),
}

impl std::fmt::Display for PersonaCardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersonaCardError::UnsupportedSchemaVersion(version) => {
                write!(f, "角色卡片 schema_version `{version}` 暂不支持")
            }
            PersonaCardError::Validation(err) => write!(f, "角色卡片校验失败：{err}"),
            PersonaCardError::VisualPackValidation(err) => {
                write!(f, "角色卡片中的展示包校验失败：{err}")
            }
            PersonaCardError::Conflict(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PersonaCardError {}

impl From<PersonaValidationError> for PersonaCardError {
    fn from(value: PersonaValidationError) -> Self {
        PersonaCardError::Validation(value)
    }
}

impl From<VisualPackValidationError> for PersonaCardError {
    fn from(value: VisualPackValidationError) -> Self {
        PersonaCardError::VisualPackValidation(value)
    }
}

impl PersonaCard {
    /// 根据当前角色和展示包快照构造一张可分享卡片。
    pub fn build(
        persona: &Persona,
        visual_pack: Option<&VisualPack>,
        export_level: PersonaCardExportLevel,
    ) -> Self {
        let resolved_visual_pack = match export_level {
            PersonaCardExportLevel::PersonaOnly => None,
            PersonaCardExportLevel::WithVisualPackRef => visual_pack.cloned(),
        };

        Self {
            schema_version: default_persona_card_schema_version(),
            exported_at: Utc::now().to_rfc3339(),
            export_level,
            persona: persona.clone(),
            visual_pack: resolved_visual_pack,
        }
    }

    /// 校验卡片结构与内嵌对象，避免导入明显错误的数据。
    pub fn validate(&self) -> Result<(), PersonaCardError> {
        let schema_version = self.schema_version.trim();
        if schema_version != PERSONA_CARD_SCHEMA_VERSION
            && schema_version != LEGACY_PERSONA_CARD_SCHEMA_VERSION
        {
            return Err(PersonaCardError::UnsupportedSchemaVersion(
                self.schema_version.clone(),
            ));
        }

        let mut persona = self.persona.clone();
        persona.migrate_legacy_defaults();
        persona.validate()?;
        if let Some(visual_pack) = &self.visual_pack {
            visual_pack.validate()?;
        }

        Ok(())
    }

    /// 基于当前本地存储与冲突策略，生成最终导入方案。
    pub fn prepare_import(
        &self,
        personas: &PersonaStore,
        visual_packs: &VisualPackStore,
        conflict_strategy: PersonaCardConflictStrategy,
    ) -> Result<PersonaCardImportPlan, PersonaCardError> {
        self.validate()?;

        let mut notices = Vec::new();
        let mut persona = self.persona.clone();
        persona.migrate_legacy_defaults();
        let mut visual_pack = self.visual_pack.clone();

        if personas.get(&persona.id).is_some() {
            match conflict_strategy {
                PersonaCardConflictStrategy::Cancel => {
                    return Err(PersonaCardError::Conflict(format!(
                        "本地已存在角色 `{}`，当前策略为 cancel，已取消导入。",
                        persona.id
                    )));
                }
                PersonaCardConflictStrategy::Overwrite => {}
                PersonaCardConflictStrategy::Rename => {
                    let original_id = persona.id.clone();
                    persona.id = next_available_id(&original_id, "-imported", |candidate| {
                        personas.get(candidate).is_some()
                    });
                    if !persona.name.trim().is_empty() {
                        persona.name = format!("{} 导入副本", persona.name.trim());
                    }
                    notices.push(format!(
                        "角色 id `{original_id}` 已存在，已自动重命名为 `{}`。",
                        persona.id
                    ));
                }
            }
        }

        if let Some(pack) = visual_pack.as_mut() {
            if pack.id.trim().is_empty() {
                return Err(PersonaCardError::Conflict(
                    "卡片声明包含展示包引用，但展示包 id 为空。".to_string(),
                ));
            }

            if visual_packs.get(&pack.id).is_some() {
                match conflict_strategy {
                    PersonaCardConflictStrategy::Cancel => {
                        return Err(PersonaCardError::Conflict(format!(
                            "本地已存在展示包 `{}`，当前策略为 cancel，已取消导入。",
                            pack.id
                        )));
                    }
                    PersonaCardConflictStrategy::Overwrite => {}
                    PersonaCardConflictStrategy::Rename => {
                        let original_pack_id = pack.id.clone();
                        pack.id = next_available_id(&original_pack_id, "-imported", |candidate| {
                            visual_packs.get(candidate).is_some()
                        });
                        if !pack.name.trim().is_empty() {
                            pack.name = format!("{} 导入副本", pack.name.trim());
                        }
                        persona.default_visual_pack_id = pack.id.clone();
                        notices.push(format!(
                            "展示包 id `{original_pack_id}` 已存在，已自动重命名为 `{}`。",
                            pack.id
                        ));
                    }
                }
            } else {
                persona.default_visual_pack_id = pack.id.clone();
            }
        } else if visual_packs.get(&persona.default_visual_pack_id).is_none() {
            notices.push(format!(
                "本地不存在展示包 `{}`，导入后将显示无图状态，直到补充对应视觉包。",
                persona.default_visual_pack_id
            ));
        }

        persona.validate()?;
        if let Some(pack) = &visual_pack {
            pack.validate()?;
        }

        Ok(PersonaCardImportPlan {
            persona,
            visual_pack,
            notices,
        })
    }
}

fn default_persona_card_schema_version() -> String {
    PERSONA_CARD_SCHEMA_VERSION.to_string()
}

fn next_available_id(base: &str, suffix: &str, exists: impl Fn(&str) -> bool) -> String {
    let trimmed_base = base.trim();
    let normalized_base = if trimmed_base.is_empty() {
        "imported"
    } else {
        trimmed_base
    };

    let first_candidate = format!("{normalized_base}{suffix}");
    if !exists(&first_candidate) {
        return first_candidate;
    }

    for index in 2..=10_000 {
        let candidate = format!("{normalized_base}{suffix}-{index}");
        if !exists(&candidate) {
            return candidate;
        }
    }

    format!("{normalized_base}{suffix}-{}", Utc::now().timestamp())
}

#[cfg(test)]
mod tests {
    use super::{
        PERSONA_CARD_SCHEMA_VERSION, PersonaCard, PersonaCardConflictStrategy, PersonaCardError,
        PersonaCardExportLevel,
    };
    use crate::domain::persona::character::store::PersonaStore;
    use crate::domain::persona::visual::VisualPack;
    use crate::domain::persona::visual::store::VisualPackStore;
    use crate::domain::persona::{Persona, RoleplayStyle, ToolPolicy};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{suffix}"))
    }

    fn sample_persona(id: &str) -> Persona {
        Persona {
            id: id.to_string(),
            name: "测试角色".to_string(),
            summary: "摘要".to_string(),
            character_profile: "冷静".to_string(),
            world_profile: "现代".to_string(),
            scenario: String::new(),
            system_prompt: "你是测试角色".to_string(),
            style: "简洁".to_string(),
            roleplay_style: RoleplayStyle::LightNarration,
            dialogue_examples: String::new(),
            author_note: String::new(),
            opening_message: String::new(),
            tool_policy: ToolPolicy::default(),
            skill_policy: Default::default(),
            mcp_policy: Default::default(),
            default_visual_pack_id: "default-visual-pack".to_string(),
            author: "rainy".to_string(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    fn sample_visual_pack(id: &str) -> VisualPack {
        VisualPack {
            id: id.to_string(),
            name: "默认展示包".to_string(),
            portrait_path: "/assets/test-character.png".to_string(),
            background_path: "/assets/test-background.png".to_string(),
            avatar_path: "/assets/test-avatar.png".to_string(),
            theme_color: "#d8596f".to_string(),
            theme_mode: "auto".to_string(),
            layout_mode: "portrait-right".to_string(),
            portrait_frame: "portrait".to_string(),
            portrait_fit: "cover".to_string(),
            portrait_position_x: 50,
            portrait_position_y: 50,
            portrait_scale: 100,
            fallback_text: "资源缺失".to_string(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn builds_persona_only_card_without_visual_pack() {
        let persona = sample_persona("persona-a");
        let visual_pack = sample_visual_pack("visual-a");

        let card = PersonaCard::build(
            &persona,
            Some(&visual_pack),
            PersonaCardExportLevel::PersonaOnly,
        );

        assert_eq!(card.schema_version, PERSONA_CARD_SCHEMA_VERSION);
        assert_eq!(card.export_level, PersonaCardExportLevel::PersonaOnly);
        assert!(card.visual_pack.is_none());
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let card = PersonaCard {
            schema_version: "legacy-card/v0".to_string(),
            exported_at: String::new(),
            export_level: PersonaCardExportLevel::PersonaOnly,
            persona: sample_persona("persona-a"),
            visual_pack: None,
        };

        let err = card.validate().expect_err("旧 schema 应被拒绝");
        assert!(matches!(
            err,
            PersonaCardError::UnsupportedSchemaVersion(version) if version == "legacy-card/v0"
        ));
    }

    #[test]
    fn prepare_import_renames_conflicting_persona_and_visual_pack() {
        let persona_dir = unique_temp_dir("muse-card-persona");
        let visual_dir = unique_temp_dir("muse-card-visual");
        let mut personas = PersonaStore::load_from_dir(&persona_dir).expect("加载角色存储失败");
        let mut visual_packs =
            VisualPackStore::load_from_dir(&visual_dir).expect("加载展示包存储失败");

        personas
            .create(sample_persona("persona-a"))
            .expect("预写入角色失败");
        visual_packs
            .upsert(sample_visual_pack("visual-a"))
            .expect("预写入展示包失败");

        let mut imported_persona = sample_persona("persona-a");
        imported_persona.default_visual_pack_id = "visual-a".to_string();
        let card = PersonaCard::build(
            &imported_persona,
            Some(&sample_visual_pack("visual-a")),
            PersonaCardExportLevel::WithVisualPackRef,
        );

        let plan = card
            .prepare_import(
                &personas,
                &visual_packs,
                PersonaCardConflictStrategy::Rename,
            )
            .expect("rename 策略应成功");

        assert_ne!(plan.persona.id, "persona-a");
        assert!(plan.persona.id.starts_with("persona-a-imported"));
        assert_eq!(
            plan.visual_pack.as_ref().map(|pack| pack.id.as_str()),
            Some(plan.persona.default_visual_pack_id.as_str())
        );
        assert_eq!(plan.notices.len(), 2);
    }

    #[test]
    fn prepare_import_cancel_rejects_conflict() {
        let persona_dir = unique_temp_dir("muse-card-cancel-persona");
        let visual_dir = unique_temp_dir("muse-card-cancel-visual");
        let mut personas = PersonaStore::load_from_dir(&persona_dir).expect("加载角色存储失败");
        let visual_packs = VisualPackStore::load_from_dir(&visual_dir).expect("加载展示包存储失败");

        personas
            .create(sample_persona("persona-a"))
            .expect("预写入角色失败");

        let card = PersonaCard::build(
            &sample_persona("persona-a"),
            None,
            PersonaCardExportLevel::PersonaOnly,
        );

        let err = card
            .prepare_import(
                &personas,
                &visual_packs,
                PersonaCardConflictStrategy::Cancel,
            )
            .expect_err("cancel 策略遇到冲突应失败");
        assert!(
            matches!(err, PersonaCardError::Conflict(message) if message.contains("已取消导入"))
        );
    }
}
