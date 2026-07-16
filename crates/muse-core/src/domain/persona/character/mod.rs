pub mod card;
pub mod store;

use serde::{Deserialize, Serialize};

/// 角色工具权限模式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyMode {
    /// 先沿用系统默认策略，后续再由运行时决定。
    #[default]
    Inherit,
    /// 禁用全部工具。
    Disabled,
    /// 仅允许白名单中的工具。
    AllowList,
}

/// 角色的工具权限配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPolicy {
    #[serde(default)]
    pub mode: ToolPolicyMode,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            mode: ToolPolicyMode::Inherit,
            allowed_tools: Vec::new(),
        }
    }
}

/// 角色对 Skill 和 MCP 资源采用的统一策略模式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePolicyMode {
    #[default]
    Inherit,
    Disabled,
    AllowList,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillPolicy {
    #[serde(default)]
    pub mode: ResourcePolicyMode,
    #[serde(default)]
    pub allowed_skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct McpPolicy {
    #[serde(default)]
    pub mode: ResourcePolicyMode,
    #[serde(default)]
    pub allowed_servers: Vec<String>,
}

/// 角色在角色扮演场景中的输出风格。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoleplayStyle {
    /// 更接近日常聊天，主要输出角色对白。
    Dialogue,
    /// 轻量加入动作、表情和环境描写。
    #[default]
    LightNarration,
    /// 更沉浸地写出动作、情绪、环境和感官细节。
    Immersive,
    /// 文字冒险式叙事，主动推进场景和事件。
    TextAdventure,
}

/// 角色资产主体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Persona {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub character_profile: String,
    #[serde(default)]
    pub world_profile: String,
    #[serde(default)]
    pub scenario: String,
    pub system_prompt: String,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub roleplay_style: RoleplayStyle,
    #[serde(default)]
    pub dialogue_examples: String,
    #[serde(default)]
    pub author_note: String,
    #[serde(default)]
    pub opening_message: String,
    #[serde(default)]
    pub tool_policy: ToolPolicy,
    #[serde(default)]
    pub skill_policy: SkillPolicy,
    #[serde(default)]
    pub mcp_policy: McpPolicy,
    pub default_visual_pack_id: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_persona_version")]
    pub version: String,
    #[serde(default)]
    pub notes: String,
}

/// 给角色列表页展示的轻量摘要。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaSummary {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub default_visual_pack_id: String,
    pub author: String,
    pub version: String,
}

/// 角色字段校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaValidationError {
    EmptyField(&'static str),
    InvalidId,
}

impl std::fmt::Display for PersonaValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersonaValidationError::EmptyField(field) => {
                write!(f, "角色字段 `{field}` 不能为空")
            }
            PersonaValidationError::InvalidId => {
                write!(f, "角色 `id` 仅允许字母、数字、短横线和下划线")
            }
        }
    }
}

impl std::error::Error for PersonaValidationError {}

impl Persona {
    /// 为早期本地数据补齐后来才成为必填项的字段。
    ///
    /// 仅在读取旧存储或旧角色卡时调用；新建和编辑仍由 `validate` 强制要求用户填写。
    pub fn migrate_legacy_defaults(&mut self) {
        if self.character_profile.trim().is_empty() {
            self.character_profile = "自然、友善，并遵循既有系统提示词中的角色设定。".to_string();
        }
        if self.world_profile.trim().is_empty() {
            self.world_profile = "现实日常".to_string();
        }
    }

    /// 校验当前角色的最小可用字段，避免把明显错误的数据写入本地存储。
    pub fn validate(&self) -> Result<(), PersonaValidationError> {
        if self.id.trim().is_empty() {
            return Err(PersonaValidationError::EmptyField("id"));
        }
        if !self
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(PersonaValidationError::InvalidId);
        }
        if self.name.trim().is_empty() {
            return Err(PersonaValidationError::EmptyField("name"));
        }
        if self.character_profile.trim().is_empty() {
            return Err(PersonaValidationError::EmptyField("character_profile"));
        }
        if self.system_prompt.trim().is_empty() {
            return Err(PersonaValidationError::EmptyField("system_prompt"));
        }
        if self.default_visual_pack_id.trim().is_empty() {
            return Err(PersonaValidationError::EmptyField("default_visual_pack_id"));
        }
        Ok(())
    }

    /// 构造列表页可复用的轻量摘要。
    pub fn summary(&self) -> PersonaSummary {
        PersonaSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            summary: self.summary.clone(),
            default_visual_pack_id: self.default_visual_pack_id.clone(),
            author: self.author.clone(),
            version: self.version.clone(),
        }
    }
}

fn default_persona_version() -> String {
    "1.0.0".to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        McpPolicy, Persona, PersonaValidationError, RoleplayStyle, SkillPolicy, ToolPolicy,
    };

    fn valid_persona() -> Persona {
        Persona {
            id: "rainy_default".to_string(),
            name: "雨灵".to_string(),
            summary: "测试角色".to_string(),
            character_profile: "冷静、可靠".to_string(),
            world_profile: "现代都市".to_string(),
            scenario: "雨夜便利店重逢".to_string(),
            system_prompt: "你是一个可靠的角色助手".to_string(),
            style: "简洁".to_string(),
            roleplay_style: RoleplayStyle::LightNarration,
            dialogue_examples: String::new(),
            author_note: String::new(),
            opening_message: String::new(),
            tool_policy: ToolPolicy::default(),
            skill_policy: SkillPolicy::default(),
            mcp_policy: McpPolicy::default(),
            default_visual_pack_id: "default-room".to_string(),
            author: String::new(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn validates_required_fields() {
        let persona = valid_persona();
        assert!(persona.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_id() {
        let mut persona = valid_persona();
        persona.id = "bad id".to_string();

        assert_eq!(persona.validate(), Err(PersonaValidationError::InvalidId));
    }

    #[test]
    fn rejects_empty_visual_pack_id() {
        let mut persona = valid_persona();
        persona.default_visual_pack_id = "   ".to_string();

        assert_eq!(
            persona.validate(),
            Err(PersonaValidationError::EmptyField("default_visual_pack_id"))
        );
    }

    #[test]
    fn rejects_empty_character_profile() {
        let mut persona = valid_persona();
        persona.character_profile = " ".to_string();

        assert_eq!(
            persona.validate(),
            Err(PersonaValidationError::EmptyField("character_profile"))
        );
    }
}
