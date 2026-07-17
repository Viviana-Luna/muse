//! 内置 Skill 注册表：随应用版本冻结的只读 Skill。
//!
//! 内置项通过 `include_str!` 编译进二进制，revision 与用户 Skill 使用同一哈希算法；
//! 用户目录中的同名 Skill 优先（遮蔽内置项）。内置项不进入管理 API。

use std::sync::OnceLock;

use super::store::{SkillSummary, parse_skill_document, revision_for};

/// 内置 Skill 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinSkill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub revision: String,
}

impl From<&BuiltinSkill> for SkillSummary {
    fn from(value: &BuiltinSkill) -> Self {
        Self {
            name: value.name.clone(),
            description: value.description.clone(),
            enabled: true,
            revision: value.revision.clone(),
            // 内置 Skill 没有磁盘修改时间；updated_at 只供管理 API 展示，内置项不进入管理 API。
            updated_at: String::new(),
        }
    }
}

const SKILL_CREATOR_DOCUMENT: &str = include_str!("builtin/skill-creator/SKILL.md");

fn builtin_documents() -> &'static [(&'static str, &'static str)] {
    &[("skill-creator", SKILL_CREATOR_DOCUMENT)]
}

static REGISTRY: OnceLock<Vec<BuiltinSkill>> = OnceLock::new();

/// 全部内置 Skill。内嵌文档在编译期确定，解析失败项会被跳过并由测试兜底。
pub fn builtin_skills() -> &'static [BuiltinSkill] {
    REGISTRY.get_or_init(|| {
        builtin_documents()
            .iter()
            .filter_map(|(expected_name, text)| {
                let parsed = parse_skill_document(text).ok()?;
                if parsed.name != *expected_name {
                    return None;
                }
                Some(BuiltinSkill {
                    name: parsed.name,
                    description: parsed.description,
                    content: parsed.content,
                    revision: revision_for(text.as_bytes(), true),
                })
            })
            .collect()
    })
}

/// 按名称查找内置 Skill。
pub fn builtin_skill(name: &str) -> Option<&'static BuiltinSkill> {
    builtin_skills().iter().find(|skill| skill.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::skill::store::MAX_SKILL_DOCUMENT_BYTES;

    #[test]
    fn registry_parses_embedded_documents() {
        let skills = builtin_skills();
        assert_eq!(skills.len(), 1, "当前只内置 skill-creator");
        let creator = &skills[0];
        assert_eq!(creator.name, "skill-creator");
        assert!(!creator.description.is_empty());
        assert!(creator.description.chars().count() <= 1024);
        assert!(creator.content.contains("Skill 创建工艺"));
        assert_eq!(creator.revision.len(), 64, "revision 应为 sha256 十六进制");
        assert!(
            SKILL_CREATOR_DOCUMENT.len() <= MAX_SKILL_DOCUMENT_BYTES,
            "内置文档不得超过运行时加载上限"
        );
    }

    #[test]
    fn lookup_by_name_and_revision_is_stable() {
        let creator = builtin_skill("skill-creator").expect("应能按名称找到内置 Skill");
        assert_eq!(creator.revision, builtin_skill("skill-creator").unwrap().revision);
        assert!(builtin_skill("missing-skill").is_none());
        assert!(crate::domain::skill::validate_skill_name(&creator.name).is_ok());
    }

    #[test]
    fn summary_conversion_marks_builtin_enabled() {
        let creator = builtin_skill("skill-creator").expect("应能按名称找到内置 Skill");
        let summary = SkillSummary::from(creator);
        assert_eq!(summary.name, "skill-creator");
        assert!(summary.enabled);
        assert_eq!(summary.revision, creator.revision);
    }
}
