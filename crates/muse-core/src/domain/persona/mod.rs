/// 角色上下文源模块：
/// - `character`：角色主体、角色卡片与角色存储。
/// - `visual`：静态展示包定义与存储。
pub mod character;
pub mod visual;

pub use character::{
    McpPolicy, Persona, PersonaFeaturePolicy, PersonaModelReference, PersonaSummary,
    PersonaValidationError, ResourcePolicyMode, RoleplayStyle, SkillPolicy, ToolPolicy,
    ToolPolicyMode,
};
pub use visual::{VisualPack, VisualPackValidationError};
