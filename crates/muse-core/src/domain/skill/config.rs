//! Agent Skills 的用户级启停覆盖配置。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::store::validate_skill_name;

/// `config.toml` 中单个 Skill 的启停覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillConfigOverride {
    pub path: String,
    pub enabled: bool,
}

/// `config.toml` 的 `[skills]` 配置段。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPreferences {
    #[serde(default)]
    pub config: Vec<SkillConfigOverride>,
}

impl SkillPreferences {
    /// 未声明覆盖的 Skill 默认启用。
    pub fn enabled_for(&self, name: &str) -> bool {
        let path = skill_document_path(name);
        self.config
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.enabled)
            .unwrap_or(true)
    }

    /// 写入显式启停状态，并清除同一路径的重复项。
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        let path = skill_document_path(name);
        self.config.retain(|entry| entry.path != path);
        self.config.push(SkillConfigOverride { path, enabled });
        self.config
            .sort_by(|left, right| left.path.cmp(&right.path));
    }

    /// 删除 Skill 时同步清理启停覆盖。
    pub fn remove(&mut self, name: &str) {
        let path = skill_document_path(name);
        self.config.retain(|entry| entry.path != path);
    }

    /// 重命名时只迁移目标 Skill 的覆盖，不触碰其他手工配置。
    pub fn rename(&mut self, current_name: &str, next_name: &str, enabled: bool) {
        self.remove(current_name);
        self.set_enabled(next_name, enabled);
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut paths = BTreeSet::new();
        for entry in &self.config {
            let name = skill_name_from_document_path(&entry.path).ok_or_else(|| {
                format!(
                    "Skill 配置路径 `{}` 必须符合 `skills/<skill-name>/SKILL.md`。",
                    entry.path
                )
            })?;
            validate_skill_name(name).map_err(|error| error.message)?;
            if !paths.insert(entry.path.as_str()) {
                return Err(format!("Skill 配置路径 `{}` 重复。", entry.path));
            }
        }
        Ok(())
    }
}

pub fn skill_document_path(name: &str) -> String {
    format!("skills/{name}/SKILL.md")
}

fn skill_name_from_document_path(path: &str) -> Option<&str> {
    let name = path.strip_prefix("skills/")?.strip_suffix("/SKILL.md")?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_enabled_and_validates_standard_paths() {
        let mut preferences = SkillPreferences::default();
        assert!(preferences.enabled_for("git-release"));
        preferences.set_enabled("git-release", false);
        assert!(!preferences.enabled_for("git-release"));
        assert!(preferences.validate().is_ok());

        preferences.config.push(SkillConfigOverride {
            path: "skills/中文/SKILL.md".to_string(),
            enabled: true,
        });
        assert!(preferences.validate().is_err());
    }
}
