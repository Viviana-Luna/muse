//! Skill 管理接口的请求 DTO。

use serde::Deserialize;

/// Skill 创建请求。
#[derive(Deserialize)]
pub struct SkillCreateRequest {
    pub name: String,
    pub description: String,
    pub content: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Skill 更新请求。
#[derive(Deserialize)]
pub struct SkillUpdateRequest {
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub revision: String,
}

fn default_true() -> bool {
    true
}
