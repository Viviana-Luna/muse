//! 用户 Agent Skills 的目录存储与跨 `config.toml` 事务。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::preferences::{MuseConfigStore, MuseConfigStoreError};
use crate::model::config::store::atomic_write_synced;

use super::config::SkillPreferences;

/// `SKILL.md` 的最大体积，与运行时加载上限保持一致。
pub const MAX_SKILL_DOCUMENT_BYTES: usize = 128 * 1024;
const LEGACY_ENABLED_MIGRATION_MARKER: &str = ".muse-migrations/skill-frontmatter-enabled-v1";
const INVALID_NAME_MESSAGE: &str =
    "Skill 名称必须为 1-64 个小写字母、数字或单连字符组合，格式如 `git-release`。";

/// Skill 列表项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub revision: String,
    pub updated_at: String,
}

/// Skill 详情。页面只编辑正文，完整 frontmatter 由服务端无损保留。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRecord {
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub revision: String,
    pub updated_at: String,
}

impl From<&SkillRecord> for SkillSummary {
    fn from(value: &SkillRecord) -> Self {
        Self {
            name: value.name.clone(),
            description: value.description.clone(),
            enabled: value.enabled,
            revision: value.revision.clone(),
            updated_at: value.updated_at.clone(),
        }
    }
}

/// Skill 创建或更新草稿；`enabled` 兼容现有 API，但不再写入 frontmatter。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDraft {
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
}

/// Skill 存储错误类型，供 API 映射稳定错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillStoreErrorKind {
    Invalid,
    NotFound,
    Conflict,
    RevisionConflict,
    Io,
}

/// Skill 存储错误。
#[derive(Debug)]
pub struct SkillStoreError {
    pub kind: SkillStoreErrorKind,
    pub message: String,
}

impl SkillStoreError {
    fn new(kind: SkillStoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn from_config(error: MuseConfigStoreError) -> Self {
        let kind = if matches!(error, MuseConfigStoreError::Conflict(_)) {
            SkillStoreErrorKind::RevisionConflict
        } else {
            SkillStoreErrorKind::Io
        };
        Self::new(kind, format!("Skill 启停配置操作失败：{error}"))
    }
}

impl std::fmt::Display for SkillStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SkillStoreError {}

/// 用户 Skill 存储。
#[derive(Debug, Clone)]
pub struct SkillStore {
    root: PathBuf,
    #[cfg(test)]
    fail_config_commit: bool,
}

impl SkillStore {
    /// 从 Muse 数据目录创建存储。
    pub fn from_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self {
            root: data_dir.as_ref().join("skills"),
            #[cfg(test)]
            fail_config_commit: false,
        }
    }

    /// 列出 Muse 用户目录中的标准 Skill。非标准目录名会产生稳定诊断，不会被静默忽略。
    pub fn list(
        &self,
        preferences: &SkillPreferences,
    ) -> Result<Vec<SkillSummary>, SkillStoreError> {
        self.ensure_root()?;
        let mut output = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            validate_skill_name(&name)?;
            let record = self.read_from_dir(&entry.path(), Some(&name), preferences)?;
            output.push(SkillSummary::from(&record));
        }
        output.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(output)
    }

    /// 读取指定 Skill。
    pub fn get(
        &self,
        name: &str,
        preferences: &SkillPreferences,
    ) -> Result<SkillRecord, SkillStoreError> {
        let normalized = validate_skill_name(name)?;
        let directory = self.skill_dir(&normalized)?;
        if !directory.exists() {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::NotFound,
                format!("Skill `{normalized}` 不存在。"),
            ));
        }
        self.read_from_dir(&directory, Some(&normalized), preferences)
    }

    /// 创建目录正文并同步写入显式启停配置。
    pub fn create(
        &self,
        draft: SkillDraft,
        config: &mut MuseConfigStore,
    ) -> Result<SkillRecord, SkillStoreError> {
        self.ensure_root()?;
        config
            .refresh_from_disk()
            .map_err(SkillStoreError::from_config)?;
        let draft = validate_draft(draft)?;
        let destination = self.skill_dir(&draft.name)?;
        if destination.exists() {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Conflict,
                format!("Skill `{}` 已存在。", draft.name),
            ));
        }

        let content = render_new_skill_document(&draft)?;
        validate_document_size(content.as_bytes())?;
        let staging = self.unique_hidden_path("staging", &draft.name);
        fs::create_dir(&staging).map_err(io_error)?;
        let result = (|| {
            atomic_write_synced(&staging.join("SKILL.md"), content.as_bytes()).map_err(io_error)?;
            fs::rename(&staging, &destination).map_err(io_error)?;

            let config_backup = config.clone();
            let mut preferences = config.skill_preferences().clone();
            preferences.set_enabled(&draft.name, draft.enabled);
            if let Err(error) = self.commit_preferences(config, preferences) {
                let rollback = fs::remove_dir_all(&destination).map_err(io_error);
                return Err(combine_rollback_error(error, rollback));
            }
            match self.read_from_dir(&destination, Some(&draft.name), config.skill_preferences()) {
                Ok(record) => Ok(record),
                Err(error) => {
                    let directory_rollback = fs::remove_dir_all(&destination).map_err(io_error);
                    let config_rollback = config
                        .restore_snapshot(&config_backup)
                        .map_err(SkillStoreError::from_config);
                    let error = combine_rollback_error(error, directory_rollback);
                    Err(combine_rollback_error(error, config_rollback))
                }
            }
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// 更新 Skill；重命名直接移动完整目录，因此辅助文件和目录不会丢失。
    pub fn update(
        &self,
        current_name: &str,
        expected_revision: &str,
        draft: SkillDraft,
        config: &mut MuseConfigStore,
    ) -> Result<SkillRecord, SkillStoreError> {
        self.ensure_root()?;
        config
            .refresh_from_disk()
            .map_err(SkillStoreError::from_config)?;
        let current_name = validate_skill_name(current_name)?;
        let current = self.get(&current_name, config.skill_preferences())?;
        if current.revision != expected_revision {
            return Err(revision_conflict());
        }
        let draft = validate_draft(draft)?;
        let source = self.skill_dir(&current_name)?;
        let destination = self.skill_dir(&draft.name)?;
        if draft.name != current_name && destination.exists() {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Conflict,
                format!("Skill `{}` 已存在。", draft.name),
            ));
        }

        let original = fs::read(source.join("SKILL.md")).map_err(io_error)?;
        let original_text = String::from_utf8(original.clone()).map_err(|_| {
            SkillStoreError::new(SkillStoreErrorKind::Invalid, "SKILL.md 必须使用 UTF-8。")
        })?;
        let parsed = parse_skill_document(&original_text)?;
        let content = render_updated_skill_document(&parsed, &draft)?;
        validate_document_size(content.as_bytes())?;

        let renamed = draft.name != current_name;
        if renamed {
            fs::rename(&source, &destination).map_err(io_error)?;
        }
        let active_path = if renamed { &destination } else { &source };
        if let Err(error) = atomic_write_synced(&active_path.join("SKILL.md"), content.as_bytes()) {
            let operation_error = io_error(error);
            let rollback = rollback_skill_update(&source, &destination, renamed, &original);
            return Err(combine_rollback_error(operation_error, rollback));
        }

        let config_backup = config.clone();
        let mut preferences = config.skill_preferences().clone();
        preferences.rename(&current_name, &draft.name, draft.enabled);
        if let Err(error) = self.commit_preferences(config, preferences) {
            let rollback = rollback_skill_update(&source, &destination, renamed, &original);
            return Err(combine_rollback_error(error, rollback));
        }

        match self.read_from_dir(active_path, Some(&draft.name), config.skill_preferences()) {
            Ok(record) => Ok(record),
            Err(error) => {
                let file_rollback =
                    rollback_skill_update(&source, &destination, renamed, &original);
                let config_rollback = config
                    .restore_snapshot(&config_backup)
                    .map_err(SkillStoreError::from_config);
                let error = combine_rollback_error(error, file_rollback);
                Err(combine_rollback_error(error, config_rollback))
            }
        }
    }

    /// 删除完整 Skill 目录并同步清理启停配置。
    pub fn delete(
        &self,
        name: &str,
        expected_revision: &str,
        config: &mut MuseConfigStore,
    ) -> Result<(), SkillStoreError> {
        self.ensure_root()?;
        config
            .refresh_from_disk()
            .map_err(SkillStoreError::from_config)?;
        let normalized = validate_skill_name(name)?;
        let current = self.get(&normalized, config.skill_preferences())?;
        if current.revision != expected_revision {
            return Err(revision_conflict());
        }
        let source = self.skill_dir(&normalized)?;
        ensure_regular_directory(&source)?;
        let deleted = self.unique_hidden_path("deleted", &normalized);
        fs::rename(&source, &deleted).map_err(io_error)?;

        let config_backup = config.clone();
        let mut preferences = config.skill_preferences().clone();
        preferences.remove(&normalized);
        if let Err(error) = self.commit_preferences(config, preferences) {
            let rollback = fs::rename(&deleted, &source).map_err(io_error);
            return Err(combine_rollback_error(error, rollback));
        }
        if let Err(error) = fs::remove_dir_all(&deleted) {
            let operation_error = io_error(error);
            let directory_rollback = fs::rename(&deleted, &source).map_err(io_error);
            let config_rollback = config
                .restore_snapshot(&config_backup)
                .map_err(SkillStoreError::from_config);
            let error = combine_rollback_error(operation_error, directory_rollback);
            return Err(combine_rollback_error(error, config_rollback));
        }
        Ok(())
    }

    fn commit_preferences(
        &self,
        config: &mut MuseConfigStore,
        preferences: SkillPreferences,
    ) -> Result<(), SkillStoreError> {
        #[cfg(test)]
        if self.fail_config_commit {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Io,
                "Skill 启停配置注入失败。",
            ));
        }
        config
            .update_skill_preferences(preferences)
            .map_err(SkillStoreError::from_config)
    }

    fn ensure_root(&self) -> Result<(), SkillStoreError> {
        fs::create_dir_all(&self.root).map_err(io_error)?;
        ensure_regular_directory(&self.root)
    }

    fn skill_dir(&self, name: &str) -> Result<PathBuf, SkillStoreError> {
        let directory = self.root.join(name);
        if directory.parent() != Some(self.root.as_path()) {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Invalid,
                "Skill 名称不能越出用户 Skill 目录。",
            ));
        }
        Ok(directory)
    }

    fn read_from_dir(
        &self,
        directory: &Path,
        expected_name: Option<&str>,
        preferences: &SkillPreferences,
    ) -> Result<SkillRecord, SkillStoreError> {
        ensure_regular_directory(directory)?;
        let path = directory.join("SKILL.md");
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SkillStoreError::new(SkillStoreErrorKind::NotFound, "Skill 缺少 SKILL.md。")
            } else {
                io_error(error)
            }
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Invalid,
                "SKILL.md 必须是普通文件，不能是符号链接。",
            ));
        }
        if metadata.len() as usize > MAX_SKILL_DOCUMENT_BYTES {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Invalid,
                "SKILL.md 超过 128KB 上限。",
            ));
        }
        let bytes = fs::read(&path).map_err(io_error)?;
        let text = String::from_utf8(bytes.clone()).map_err(|_| {
            SkillStoreError::new(SkillStoreErrorKind::Invalid, "SKILL.md 必须使用 UTF-8。")
        })?;
        let parsed = parse_skill_document(&text)?;
        if let Some(expected_name) = expected_name
            && parsed.name != expected_name
        {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Invalid,
                format!(
                    "Skill 目录名 `{expected_name}` 与 frontmatter name `{}` 不一致。",
                    parsed.name
                ),
            ));
        }
        let enabled = preferences.enabled_for(&parsed.name);
        let revision = revision_for(&bytes, enabled);
        let updated_at = metadata
            .modified()
            .ok()
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(Utc::now)
            .to_rfc3339();
        Ok(SkillRecord {
            name: parsed.name,
            description: parsed.description,
            content: parsed.content,
            enabled,
            revision,
            updated_at,
        })
    }

    fn unique_hidden_path(&self, kind: &str, name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        self.root.join(format!(".{kind}-{name}-{nanos}"))
    }

    #[cfg(test)]
    fn inject_config_commit_failure(&mut self) {
        self.fail_config_commit = true;
    }
}

/// 把合法旧 Skill 的 `enabled` 迁入 `config.toml`，验证后再移除旧 frontmatter 字段。
pub fn migrate_legacy_skill_enabled(
    data_dir: impl AsRef<Path>,
    config: &mut MuseConfigStore,
) -> Result<(), SkillStoreError> {
    let data_dir = data_dir.as_ref();
    let marker = data_dir.join(LEGACY_ENABLED_MIGRATION_MARKER);
    if marker.is_file() {
        return Ok(());
    }
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let store = SkillStore::from_data_dir(data_dir);
    store.ensure_root()?;
    let mut migrations = Vec::new();
    for entry in fs::read_dir(&store.root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || validate_skill_name(&name).is_err() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let path = entry.path().join("SKILL.md");
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path).map_err(io_error)?;
        let Ok(text) = String::from_utf8(bytes.clone()) else {
            continue;
        };
        let Ok(parsed) = parse_skill_document(&text) else {
            continue;
        };
        if parsed.name != name {
            continue;
        }
        if let Some(enabled) = parsed.legacy_enabled {
            migrations.push((name, enabled, path, bytes, remove_legacy_enabled(&text)?));
        }
    }

    let config_backup = config.clone();
    let mut preferences = config.skill_preferences().clone();
    for (name, enabled, _, _, _) in &migrations {
        preferences.set_enabled(name, *enabled);
    }
    if !migrations.is_empty() {
        config
            .update_skill_preferences(preferences.clone())
            .map_err(SkillStoreError::from_config)?;
        let reloaded = match MuseConfigStore::load_from_dir(data_dir) {
            Ok(reloaded) => reloaded,
            Err(error) => {
                config
                    .restore_snapshot(&config_backup)
                    .map_err(SkillStoreError::from_config)?;
                return Err(SkillStoreError::from_config(error));
            }
        };
        if reloaded.skill_preferences() != &preferences {
            config
                .restore_snapshot(&config_backup)
                .map_err(SkillStoreError::from_config)?;
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Io,
                "Skill 启停配置回读校验失败。",
            ));
        }
    }

    let mut written: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for (_, _, path, original, migrated) in &migrations {
        if let Err(error) = atomic_write_synced(path, migrated.as_bytes()) {
            for (written_path, written_original) in written.iter().rev() {
                let _ = atomic_write_synced(written_path, written_original);
            }
            if !migrations.is_empty() {
                let _ = config.restore_snapshot(&config_backup);
            }
            return Err(io_error(error));
        }
        written.push((path.clone(), original.clone()));
    }

    if let Err(error) = atomic_write_synced(&marker, b"skill-frontmatter-enabled-v1\n") {
        for (path, original) in written.iter().rev() {
            let _ = atomic_write_synced(path, original);
        }
        if !migrations.is_empty() {
            let _ = config.restore_snapshot(&config_backup);
        }
        return Err(io_error(error));
    }
    Ok(())
}

#[derive(Debug)]
struct ParsedSkillDocument {
    name: String,
    description: String,
    content: String,
    frontmatter_lines: Vec<String>,
    legacy_enabled: Option<bool>,
}

fn validate_draft(mut draft: SkillDraft) -> Result<SkillDraft, SkillStoreError> {
    draft.name = validate_skill_name(&draft.name)?;
    draft.description = draft.description.trim().to_string();
    draft.content = draft.content.trim().to_string();
    validate_description(&draft.description)?;
    if draft.content.is_empty() {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "SKILL.md 正文不能为空。",
        ));
    }
    Ok(draft)
}

fn validate_description(description: &str) -> Result<(), SkillStoreError> {
    if description.trim().is_empty() {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "Skill description 不能为空。",
        ));
    }
    if description.chars().count() > 1024 {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "Skill description 不能超过 1024 个字符。",
        ));
    }
    Ok(())
}

/// Agent Skills 标准名称校验，前后端使用同一规则和中文提示。
pub fn validate_skill_name(value: &str) -> Result<String, SkillStoreError> {
    let value = value.trim();
    let valid = (1..=64).contains(&value.len())
        && value.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if !valid {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            INVALID_NAME_MESSAGE,
        ));
    }
    Ok(value.to_string())
}

fn ensure_regular_directory(path: &Path) -> Result<(), SkillStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "Skill 存储路径必须是普通目录，不能是符号链接。",
        ));
    }
    Ok(())
}

fn render_new_skill_document(draft: &SkillDraft) -> Result<String, SkillStoreError> {
    let name = yaml_string(&draft.name)?;
    let description = yaml_string(&draft.description)?;
    Ok(format!(
        "---\nname: {name}\ndescription: {description}\n---\n{}\n",
        draft.content
    ))
}

fn render_updated_skill_document(
    parsed: &ParsedSkillDocument,
    draft: &SkillDraft,
) -> Result<String, SkillStoreError> {
    let name = yaml_string(&draft.name)?;
    let description = yaml_string(&draft.description)?;
    let mut lines = Vec::new();
    for line in &parsed.frontmatter_lines {
        match top_level_key(line) {
            Some("name") => lines.push(format!("name: {name}")),
            Some("description") => lines.push(format!("description: {description}")),
            Some("enabled") => {}
            _ => lines.push(line.clone()),
        }
    }
    Ok(format!(
        "---\n{}\n---\n{}\n",
        lines.join("\n"),
        draft.content
    ))
}

fn parse_skill_document(text: &str) -> Result<ParsedSkillDocument, SkillStoreError> {
    let normalized = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "SKILL.md 缺少 frontmatter。",
        ));
    };
    let Some((frontmatter, content)) = rest.split_once("\n---\n") else {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "SKILL.md frontmatter 未正确结束。",
        ));
    };
    let mut name = None;
    let mut description = None;
    let mut legacy_enabled = None;
    let frontmatter_lines = frontmatter.lines().map(str::to_string).collect::<Vec<_>>();
    for line in &frontmatter_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(SkillStoreError::new(
                SkillStoreErrorKind::Invalid,
                "SKILL.md frontmatter 包含无效的顶层字段。",
            ));
        };
        match key.trim() {
            "name" => set_once(&mut name, parse_yaml_string(value, "name")?, "name")?,
            "description" => set_once(
                &mut description,
                parse_yaml_string(value, "description")?,
                "description",
            )?,
            "enabled" => {
                let parsed = value.trim().parse::<bool>().map_err(|_| {
                    SkillStoreError::new(
                        SkillStoreErrorKind::Invalid,
                        "旧 Skill enabled 必须是 true 或 false。",
                    )
                })?;
                set_once(&mut legacy_enabled, parsed, "enabled")?;
            }
            // 标准可选字段和未来扩展字段均由页面原样保留，不在 Muse 中解释。
            _ => {}
        }
    }
    let name = validate_skill_name(&name.ok_or_else(|| {
        SkillStoreError::new(SkillStoreErrorKind::Invalid, "SKILL.md 缺少 name。")
    })?)?;
    let description = description.ok_or_else(|| {
        SkillStoreError::new(SkillStoreErrorKind::Invalid, "SKILL.md 缺少 description。")
    })?;
    validate_description(&description)?;
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "SKILL.md 正文不能为空。",
        ));
    }
    Ok(ParsedSkillDocument {
        name,
        description,
        content,
        frontmatter_lines,
        legacy_enabled,
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, field: &str) -> Result<(), SkillStoreError> {
    if slot.replace(value).is_some() {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            format!("SKILL.md frontmatter 的 `{field}` 不能重复。"),
        ));
    }
    Ok(())
}

fn top_level_key(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t']) {
        return None;
    }
    line.split_once(':').map(|(key, _)| key.trim())
}

fn parse_yaml_string(value: &str, field: &str) -> Result<String, SkillStoreError> {
    let value = value.trim();
    let parsed = if value.starts_with('"') {
        serde_json::from_str(value).map_err(|_| invalid_frontmatter_string(field))?
    } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value
            .split_once(" #")
            .map(|(plain, _)| plain)
            .unwrap_or(value)
            .trim()
            .to_string()
    };
    if parsed.is_empty() {
        return Err(invalid_frontmatter_string(field));
    }
    Ok(parsed)
}

fn invalid_frontmatter_string(field: &str) -> SkillStoreError {
    SkillStoreError::new(
        SkillStoreErrorKind::Invalid,
        format!("SKILL.md frontmatter 的 `{field}` 必须是单行字符串。"),
    )
}

fn yaml_string(value: &str) -> Result<String, SkillStoreError> {
    serde_json::to_string(value).map_err(|error| {
        SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            format!("Skill frontmatter 序列化失败：{error}"),
        )
    })
}

fn remove_legacy_enabled(text: &str) -> Result<String, SkillStoreError> {
    let parsed = parse_skill_document(text)?;
    let mut output = Vec::new();
    for line in parsed.frontmatter_lines {
        if top_level_key(&line) != Some("enabled") {
            output.push(line);
        }
    }
    Ok(format!(
        "---\n{}\n---\n{}",
        output.join("\n"),
        text.replace("\r\n", "\n")
            .split_once("\n---\n")
            .expect("已解析 frontmatter")
            .1
    ))
}

fn validate_document_size(bytes: &[u8]) -> Result<(), SkillStoreError> {
    if bytes.len() > MAX_SKILL_DOCUMENT_BYTES {
        return Err(SkillStoreError::new(
            SkillStoreErrorKind::Invalid,
            "SKILL.md 超过 128KB 上限。",
        ));
    }
    Ok(())
}

fn revision_for(bytes: &[u8], enabled: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest.update(if enabled {
        b"\0enabled=1"
    } else {
        b"\0enabled=0"
    });
    format!("{:x}", digest.finalize())
}

fn rollback_skill_update(
    source: &Path,
    destination: &Path,
    renamed: bool,
    original: &[u8],
) -> Result<(), SkillStoreError> {
    if renamed {
        fs::rename(destination, source).map_err(io_error)?;
    }
    atomic_write_synced(&source.join("SKILL.md"), original).map_err(io_error)
}

fn revision_conflict() -> SkillStoreError {
    SkillStoreError::new(
        SkillStoreErrorKind::RevisionConflict,
        "Skill 已在应用外部或其他窗口中变更，请刷新后重试。",
    )
}

fn combine_rollback_error(
    operation: SkillStoreError,
    rollback: Result<(), SkillStoreError>,
) -> SkillStoreError {
    match rollback {
        Ok(()) => operation,
        Err(rollback) => SkillStoreError::new(
            SkillStoreErrorKind::Io,
            format!(
                "{}；自动回滚也失败：{}",
                operation.message, rollback.message
            ),
        ),
    }
}

fn io_error(error: std::io::Error) -> SkillStoreError {
    SkillStoreError::new(
        SkillStoreErrorKind::Io,
        format!("Skill 存储操作失败：{error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "muse-skill-store-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).expect("应能创建测试目录");
        path
    }

    fn draft(name: &str) -> SkillDraft {
        SkillDraft {
            name: name.to_string(),
            description: "用于测试的 Skill。".to_string(),
            content: "# 规则\n\n按说明执行。".to_string(),
            enabled: true,
        }
    }

    fn config(root: &Path) -> MuseConfigStore {
        MuseConfigStore::load_from_dir(root).expect("应加载测试配置")
    }

    #[test]
    fn creates_renames_and_deletes_full_skill_directory() {
        let root = temp_dir();
        let mut config = config(&root);
        let store = SkillStore::from_data_dir(&root);
        let created = store.create(draft("writing-helper"), &mut config).unwrap();
        fs::create_dir_all(root.join("skills/writing-helper/scripts")).unwrap();
        fs::write(root.join("skills/writing-helper/scripts/run.sh"), "echo ok").unwrap();

        let mut renamed = draft("article-polish");
        renamed.enabled = false;
        let updated = store
            .update("writing-helper", &created.revision, renamed, &mut config)
            .unwrap();
        assert!(!updated.enabled);
        assert!(root.join("skills/article-polish/scripts/run.sh").is_file());
        assert!(!root.join("skills/writing-helper").exists());
        assert!(!config.skill_preferences().enabled_for("article-polish"));

        store
            .delete("article-polish", &updated.revision, &mut config)
            .unwrap();
        assert!(store.list(config.skill_preferences()).unwrap().is_empty());
        assert!(config.skill_preferences().config.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enforces_standard_names_and_directory_frontmatter_match() {
        for invalid in [
            "",
            "中文",
            "Upper",
            "snake_case",
            "-alpha",
            "alpha-",
            "a--b",
        ] {
            assert_eq!(
                validate_skill_name(invalid).unwrap_err().message,
                INVALID_NAME_MESSAGE
            );
        }
        assert!(validate_skill_name("a").is_ok());
        assert!(validate_skill_name(&"a".repeat(64)).is_ok());
        assert!(validate_skill_name(&"a".repeat(65)).is_err());

        let root = temp_dir();
        let store = SkillStore::from_data_dir(&root);
        fs::create_dir_all(root.join("skills/alpha")).unwrap();
        fs::write(
            root.join("skills/alpha/SKILL.md"),
            "---\nname: beta\ndescription: mismatch\n---\nbody\n",
        )
        .unwrap();
        let error = store.list(&SkillPreferences::default()).unwrap_err();
        assert!(error.message.contains("不一致"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preserves_standard_optional_unknown_frontmatter_and_auxiliary_files() {
        let root = temp_dir();
        let mut config = config(&root);
        let store = SkillStore::from_data_dir(&root);
        fs::create_dir_all(root.join("skills/release-helper/references")).unwrap();
        fs::write(
            root.join("skills/release-helper/references/checklist.md"),
            "保留",
        )
        .unwrap();
        fs::write(
            root.join("skills/release-helper/SKILL.md"),
            "---\nname: release-helper\ndescription: 发布助手\nlicense: MIT\ncompatibility: Muse 1\nmetadata:\n  author: luna\nfuture-field: keep\n---\n# 旧正文\n",
        )
        .unwrap();
        let current = store
            .get("release-helper", config.skill_preferences())
            .unwrap();
        let mut next = draft("release-helper");
        next.description = "新描述".to_string();
        store
            .update("release-helper", &current.revision, next, &mut config)
            .unwrap();
        let saved = fs::read_to_string(root.join("skills/release-helper/SKILL.md")).unwrap();
        assert!(saved.contains("license: MIT"));
        assert!(saved.contains("compatibility: Muse 1"));
        assert!(saved.contains("metadata:\n  author: luna"));
        assert!(saved.contains("future-field: keep"));
        assert!(!saved.contains("enabled:"));
        assert!(
            root.join("skills/release-helper/references/checklist.md")
                .is_file()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enabled_state_participates_in_revision_and_stale_drafts_conflict() {
        let root = temp_dir();
        let mut config = config(&root);
        let store = SkillStore::from_data_dir(&root);
        let created = store.create(draft("revision-test"), &mut config).unwrap();
        let mut preferences = config.skill_preferences().clone();
        preferences.set_enabled("revision-test", false);
        config.update_skill_preferences(preferences).unwrap();
        let changed = store
            .get("revision-test", config.skill_preferences())
            .unwrap();
        assert_ne!(created.revision, changed.revision);
        assert_eq!(
            store
                .update(
                    "revision-test",
                    &created.revision,
                    draft("revision-test"),
                    &mut config
                )
                .unwrap_err()
                .kind,
            SkillStoreErrorKind::RevisionConflict
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_conflicts_oversized_documents_and_manual_revision_changes() {
        let root = temp_dir();
        let mut config = config(&root);
        let store = SkillStore::from_data_dir(&root);
        let created = store.create(draft("conflict-test"), &mut config).unwrap();
        assert_eq!(
            store
                .create(draft("conflict-test"), &mut config)
                .unwrap_err()
                .kind,
            SkillStoreErrorKind::Conflict
        );

        let path = root.join("skills/conflict-test/SKILL.md");
        let manual = fs::read_to_string(&path)
            .unwrap()
            .replace("按说明执行。", "按手工更新后的说明执行。");
        fs::write(&path, manual).unwrap();
        assert_eq!(
            store
                .update(
                    "conflict-test",
                    &created.revision,
                    draft("conflict-test"),
                    &mut config,
                )
                .unwrap_err()
                .kind,
            SkillStoreErrorKind::RevisionConflict
        );

        let current = store
            .get("conflict-test", config.skill_preferences())
            .unwrap();
        let mut oversized = draft("conflict-test");
        oversized.content = "x".repeat(MAX_SKILL_DOCUMENT_BYTES);
        assert_eq!(
            store
                .update("conflict-test", &current.revision, oversized, &mut config,)
                .unwrap_err()
                .kind,
            SkillStoreErrorKind::Invalid
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manual_toml_enable_change_invalidates_page_revision() {
        let root = temp_dir();
        let mut config = config(&root);
        let store = SkillStore::from_data_dir(&root);
        let created = store.create(draft("manual-config"), &mut config).unwrap();
        let config_path = root.join("config.toml");
        let manual = fs::read_to_string(&config_path)
            .unwrap()
            .replace("enabled = true", "enabled = false");
        fs::write(&config_path, manual).unwrap();

        assert_eq!(
            store
                .update(
                    "manual-config",
                    &created.revision,
                    draft("manual-config"),
                    &mut config,
                )
                .unwrap_err()
                .kind,
            SkillStoreErrorKind::RevisionConflict
        );
        let reloaded = store
            .get("manual-config", config.skill_preferences())
            .unwrap();
        assert!(!reloaded.enabled);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rolls_back_document_and_rename_when_config_commit_fails() {
        let root = temp_dir();
        let mut config = config(&root);
        let mut store = SkillStore::from_data_dir(&root);
        let created = store.create(draft("rollback-source"), &mut config).unwrap();
        let original = fs::read(root.join("skills/rollback-source/SKILL.md")).unwrap();
        store.inject_config_commit_failure();
        let error = store
            .update(
                "rollback-source",
                &created.revision,
                draft("rollback-target"),
                &mut config,
            )
            .unwrap_err();
        assert_eq!(error.kind, SkillStoreErrorKind::Io);
        assert_eq!(
            fs::read(root.join("skills/rollback-source/SKILL.md")).unwrap(),
            original
        );
        assert!(!root.join("skills/rollback-target").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migrates_legacy_enabled_after_toml_verification() {
        let root = temp_dir();
        fs::create_dir_all(root.join("skills/legacy-skill")).unwrap();
        fs::write(
            root.join("skills/legacy-skill/SKILL.md"),
            "---\nname: legacy-skill\ndescription: 旧技能\nenabled: false\nlicense: MIT\n---\n正文\n",
        )
        .unwrap();
        let mut config = config(&root);
        migrate_legacy_skill_enabled(&root, &mut config).unwrap();
        let saved = fs::read_to_string(root.join("skills/legacy-skill/SKILL.md")).unwrap();
        assert!(!saved.contains("enabled:"));
        assert!(saved.contains("license: MIT"));
        assert!(!config.skill_preferences().enabled_for("legacy-skill"));
        assert!(root.join(LEGACY_ENABLED_MIGRATION_MARKER).is_file());
        migrate_legacy_skill_enabled(&root, &mut config).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_skips_malformed_skill_but_runtime_returns_diagnostic() {
        let root = temp_dir();
        fs::create_dir_all(root.join("skills/malformed-skill")).unwrap();
        fs::write(
            root.join("skills/malformed-skill/SKILL.md"),
            "---\nname: malformed-skill\nenabled: false\n---\n正文\n",
        )
        .unwrap();
        let mut config = config(&root);
        migrate_legacy_skill_enabled(&root, &mut config)
            .expect("损坏 Skill 不应阻止应用完成启动迁移");
        let store = SkillStore::from_data_dir(&root);
        let error = store.list(config.skill_preferences()).unwrap_err();
        assert_eq!(error.kind, SkillStoreErrorKind::Invalid);
        assert!(error.message.contains("缺少 description"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinked_skill_directories_but_rejects_invalid_real_names() {
        use std::os::unix::fs::symlink;
        let root = temp_dir();
        let outside = temp_dir();
        let store = SkillStore::from_data_dir(&root);
        fs::create_dir_all(root.join("skills")).unwrap();
        symlink(&outside, root.join("skills/linked")).unwrap();
        assert!(store.list(&SkillPreferences::default()).unwrap().is_empty());
        fs::create_dir_all(root.join("skills/测试")).unwrap();
        assert_eq!(
            store.list(&SkillPreferences::default()).unwrap_err().kind,
            SkillStoreErrorKind::Invalid
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
