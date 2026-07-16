//! 启动期角色数据迁移，只处理能够机械证明安全的历史自动默认角色。

use std::io::Write;
use std::path::{Path, PathBuf};

use muse_core::domain::persona::character::store::PersonaStore;
use muse_core::domain::persona::{Persona, RoleplayStyle, ToolPolicy};
use muse_core::domain::usage::inspect_runtime_history_evidence_read_only;
use muse_runtime::session::SessionStore;
use sha2::{Digest, Sha256};

const LEGACY_DEFAULT_PERSONA_ID: &str = "default-persona";
const LEGACY_DEFAULT_PERSONA_BACKUP_FILE: &str = "personas.v0.1.0-auto-default.backup.json";
const LEGACY_DEFAULT_PERSONA_COMPLETED_FILE: &str = "personas.v0.1.0-auto-default.completed";
const LEGACY_DEFAULT_PERSONA_COMPLETED_SCHEMA: &str = "muse-default-persona-migration/v1";

/// 历史自动默认角色迁移结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyDefaultPersonaMigrationOutcome {
    NotPresent,
    AlreadyCompleted,
    PreservedTemplateMismatch,
    PreservedHistory,
    PreservedUncertain { reason: String },
    Removed { backup_path: PathBuf },
}

/// 仅在全字段严格匹配历史模板、canonical 会话为空、无隔离记录且运行时历史为空时删除。
///
/// 调用方必须已持有 data-dir 实例锁，并在 `SessionStore::open` 完成 legacy 聚合后调用。
pub async fn migrate_pristine_legacy_default_persona(
    data_dir: &Path,
    session_store: &SessionStore,
    quarantined_session_records: usize,
) -> Result<LegacyDefaultPersonaMigrationOutcome, String> {
    let persona_dir = data_dir.join("personas");
    let persona_path = persona_dir.join("personas.json");
    let backup_path = persona_dir.join(LEGACY_DEFAULT_PERSONA_BACKUP_FILE);
    let completed_path = persona_dir.join(LEGACY_DEFAULT_PERSONA_COMPLETED_FILE);
    match read_completed_marker(&completed_path)? {
        Some(true) => return Ok(LegacyDefaultPersonaMigrationOutcome::AlreadyCompleted),
        Some(false) => {
            return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain {
                reason: "旧默认角色迁移完成标记无效，已拒绝再次修改角色库".to_string(),
            });
        }
        None => {}
    }

    let mut personas = PersonaStore::load_from_dir(data_dir)
        .map_err(|error| format!("加载角色库以检查旧默认角色失败：{error}"))?;
    let Some(candidate) = personas.get(LEGACY_DEFAULT_PERSONA_ID).cloned() else {
        if personas.personas().is_empty() && durable_backup_matches_template(&backup_path)? {
            create_or_verify_durable_marker(&completed_path, &backup_path)?;
            return Ok(LegacyDefaultPersonaMigrationOutcome::AlreadyCompleted);
        }
        return Ok(LegacyDefaultPersonaMigrationOutcome::NotPresent);
    };
    if candidate != historical_default_persona() {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedTemplateMismatch);
    }

    let metadata = std::fs::symlink_metadata(&persona_path).map_err(|error| {
        format!(
            "读取旧默认角色源文件 `{}` 元数据失败：{error}",
            persona_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain {
            reason: "角色源文件不是普通文件，已拒绝迁移".to_string(),
        });
    }
    let original_bytes = std::fs::read(&persona_path).map_err(|error| {
        format!(
            "逐字节读取旧默认角色源文件 `{}` 失败：{error}",
            persona_path.display()
        )
    })?;
    if !raw_store_matches_historical_template(&original_bytes) {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedTemplateMismatch);
    }
    if quarantined_session_records > 0 {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain {
            reason: format!(
                "canonical 会话存储仍有 {quarantined_session_records} 条隔离记录，无法证明未引用旧默认角色"
            ),
        });
    }

    let events = match session_store.aggregate_events().await {
        Ok(events) => events,
        Err(error) => {
            return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain {
                reason: format!("读取 canonical 会话事实失败：{error}"),
            });
        }
    };
    if !events.is_empty() {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedHistory);
    }

    let runtime_evidence = match inspect_runtime_history_evidence_read_only(data_dir) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain {
                reason: format!("读取运行时历史库失败：{error}"),
            });
        }
    };
    if runtime_evidence.has_history {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedHistory);
    }
    if !runtime_evidence.unknown_tables.is_empty() {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain {
            reason: format!(
                "运行时数据库存在未知数据表：{}",
                runtime_evidence.unknown_tables.join(", ")
            ),
        });
    }
    if let Some(reason) = inspect_filesystem_history_evidence(data_dir, session_store)? {
        return Ok(LegacyDefaultPersonaMigrationOutcome::PreservedUncertain { reason });
    }
    create_or_verify_durable_backup(&backup_path, &original_bytes).map_err(|error| {
        format!(
            "为旧默认角色创建耐久备份 `{}` 失败：{error}",
            backup_path.display()
        )
    })?;

    if !personas.delete(LEGACY_DEFAULT_PERSONA_ID) {
        return Ok(LegacyDefaultPersonaMigrationOutcome::NotPresent);
    }
    personas
        .save()
        .map_err(|error| format!("原子保存旧默认角色迁移结果失败：{error}"))?;
    let verified = PersonaStore::load_from_dir(data_dir)
        .map_err(|error| format!("回读旧默认角色迁移结果失败：{error}"))?;
    if !verified.personas().is_empty() || verified.active_persona_id().is_some() {
        return Err("旧默认角色迁移结果回读不为空，已停止发布完成标记".to_string());
    }
    create_or_verify_durable_marker(&completed_path, &backup_path)?;
    Ok(LegacyDefaultPersonaMigrationOutcome::Removed { backup_path })
}

fn read_completed_marker(path: &Path) -> Result<Option<bool>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取旧默认角色迁移标记失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(Some(false));
    }
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("读取旧默认角色迁移标记失败：{error}"))?;
    Ok(Some(content.starts_with(&format!(
        "{LEGACY_DEFAULT_PERSONA_COMPLETED_SCHEMA}\n"
    ))))
}

fn durable_backup_matches_template(path: &Path) -> Result<bool, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("读取旧默认角色备份元数据失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    let content =
        std::fs::read(path).map_err(|error| format!("读取旧默认角色备份失败：{error}"))?;
    Ok(raw_store_matches_historical_template(&content))
}

fn create_or_verify_durable_marker(path: &Path, backup_path: &Path) -> Result<(), String> {
    let backup = std::fs::read(backup_path)
        .map_err(|error| format!("读取旧默认角色备份以生成完成标记失败：{error}"))?;
    let checksum = format!("{:x}", Sha256::digest(&backup));
    let content = format!("{LEGACY_DEFAULT_PERSONA_COMPLETED_SCHEMA}\nbackup_sha256={checksum}\n");
    create_or_verify_durable_backup(path, content.as_bytes())
        .map_err(|error| format!("发布旧默认角色迁移完成标记失败：{error}"))
}

fn historical_default_persona() -> Persona {
    Persona {
        id: LEGACY_DEFAULT_PERSONA_ID.to_string(),
        name: "小灵".to_string(),
        summary: "从当前本地配置迁移的默认角色".to_string(),
        character_profile: "友善、可靠，并保持与用户的角色互动。".to_string(),
        world_profile: "现实日常".to_string(),
        scenario: String::new(),
        system_prompt: "请稳定扮演当前角色，与用户进行持续对话。".to_string(),
        style: String::new(),
        roleplay_style: RoleplayStyle::LightNarration,
        dialogue_examples: String::new(),
        author_note: String::new(),
        opening_message: String::new(),
        tool_policy: ToolPolicy::default(),
        skill_policy: Default::default(),
        mcp_policy: Default::default(),
        default_visual_pack_id: "default-visual-pack".to_string(),
        author: "system".to_string(),
        version: "1.0.0".to_string(),
        notes: "系统自动生成的默认角色，可在后续编辑中覆盖。".to_string(),
    }
}

/// serde 默认值会把缺失字段补成“看似相等”；迁移必须同时验证原始 key 集和原始值。
fn raw_store_matches_historical_template(content: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(content) else {
        return false;
    };
    let Some(store) = value.as_object() else {
        return false;
    };
    if store.len() != 2
        || !store.contains_key("personas")
        || !store.contains_key("active_persona_id")
        || store
            .get("active_persona_id")
            .and_then(|value| value.as_str())
            != Some(LEGACY_DEFAULT_PERSONA_ID)
    {
        return false;
    }
    let Some(personas) = store.get("personas").and_then(|value| value.as_array()) else {
        return false;
    };
    if personas.len() != 1 {
        return false;
    }
    let Some(raw_persona) = personas[0].as_object() else {
        return false;
    };
    let expected_keys = [
        "id",
        "name",
        "summary",
        "character_profile",
        "world_profile",
        "scenario",
        "system_prompt",
        "style",
        "roleplay_style",
        "dialogue_examples",
        "author_note",
        "opening_message",
        "tool_policy",
        "default_visual_pack_id",
        "author",
        "version",
        "notes",
    ]
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    let actual_keys = raw_persona
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if actual_keys != expected_keys {
        return false;
    }
    let Ok(mut expected) = serde_json::to_value(historical_default_persona()) else {
        return false;
    };
    let Some(expected) = expected.as_object_mut() else {
        return false;
    };
    // v0.1.0 的磁盘模板早于 Skill/MCP 角色策略；缺失字段只用于反序列化兼容，
    // 不能把当前结构重新序列化后冒充旧模板。
    expected.remove("skill_policy");
    expected.remove("mcp_policy");
    personas[0] == serde_json::Value::Object(expected.clone())
}

fn inspect_filesystem_history_evidence(
    data_dir: &Path,
    session_store: &SessionStore,
) -> Result<Option<String>, String> {
    if path_exists_without_following(&data_dir.join("harness"))? {
        return Ok(Some(
            "存在 harness 工具结果或命令审计目录，无法证明旧默认角色未产生副作用".to_string(),
        ));
    }
    if let Some(reason) = inspect_session_storage_evidence(session_store)? {
        return Ok(Some(reason));
    }
    inspect_runtime_storage_evidence(data_dir)
}

fn inspect_session_storage_evidence(
    session_store: &SessionStore,
) -> Result<Option<String>, String> {
    let sessions_dir = session_store.sessions_dir();
    let allowed_root = std::collections::BTreeSet::from(["store.json", "generations"]);
    for entry in read_directory_entries(sessions_dir)? {
        let name = entry_name(&entry)?;
        if !allowed_root.contains(name.as_str()) {
            return Ok(Some(format!(
                "会话存储存在 legacy、备份、暂存或未知证据 `{name}`"
            )));
        }
    }

    let generations_dir = sessions_dir.join("generations");
    let generation_entries = read_directory_entries(&generations_dir)?;
    if generation_entries.len() != 1 {
        return Ok(Some(format!(
            "会话存储包含 {} 个 generation，无法证明旧 generation 没有历史",
            generation_entries.len()
        )));
    }
    let generation = &generation_entries[0];
    let generation_name = entry_name(generation)?;
    if generation_name != session_store.generation_id() {
        return Ok(Some(
            "活动 generation 与磁盘唯一 generation 不一致".to_string(),
        ));
    }
    let generation_metadata = generation
        .metadata()
        .map_err(|error| format!("读取 generation 元数据失败：{error}"))?;
    if !generation_metadata.is_dir() || generation.file_type().is_ok_and(|kind| kind.is_symlink()) {
        return Ok(Some("活动 generation 不是普通目录".to_string()));
    }

    let generation_dir = generation.path();
    let allowed_generation = std::collections::BTreeSet::from([
        "manifest.json",
        "runtime-manifest.json",
        "conversations",
    ]);
    for entry in read_directory_entries(&generation_dir)? {
        let name = entry_name(&entry)?;
        if !allowed_generation.contains(name.as_str()) {
            return Ok(Some(format!(
                "活动 generation 存在隔离、暂存或未知证据 `{name}`"
            )));
        }
    }
    let conversations_dir = generation_dir.join("conversations");
    if !read_directory_entries(&conversations_dir)?.is_empty() {
        return Ok(Some(
            "活动 generation 的 conversations 目录仍有会话文件".to_string(),
        ));
    }
    Ok(None)
}

fn inspect_runtime_storage_evidence(data_dir: &Path) -> Result<Option<String>, String> {
    let runtime_dir = data_dir.join("runtime");
    if !path_exists_without_following(&runtime_dir)? {
        return Ok(None);
    }
    let allowed_runtime = std::collections::BTreeSet::from(["muse.sqlite", "chat-requests"]);
    for entry in read_directory_entries(&runtime_dir)? {
        let name = entry_name(&entry)?;
        if !allowed_runtime.contains(name.as_str()) {
            return Ok(Some(format!("runtime 目录存在未知历史证据 `{name}`")));
        }
        if name == "chat-requests" {
            let metadata = entry
                .metadata()
                .map_err(|error| format!("读取 chat-requests 元数据失败：{error}"))?;
            if !metadata.is_dir()
                || entry
                    .file_type()
                    .is_ok_and(|file_type| file_type.is_symlink())
            {
                return Ok(Some("chat-requests 不是普通目录".to_string()));
            }
            if !read_directory_entries(&entry.path())?.is_empty() {
                return Ok(Some(
                    "chat-requests 中存在已受理请求事实，已保留旧默认角色".to_string(),
                ));
            }
        } else {
            let metadata = entry
                .metadata()
                .map_err(|error| format!("读取运行时数据库元数据失败：{error}"))?;
            if !metadata.is_file()
                || entry
                    .file_type()
                    .is_ok_and(|file_type| file_type.is_symlink())
            {
                return Ok(Some("运行时数据库不是普通文件".to_string()));
            }
        }
    }
    Ok(None)
}

fn read_directory_entries(path: &Path) -> Result<Vec<std::fs::DirEntry>, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("读取目录 `{}` 元数据失败：{error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("路径 `{}` 不是普通目录", path.display()));
    }
    std::fs::read_dir(path)
        .map_err(|error| format!("读取目录 `{}` 失败：{error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("遍历目录 `{}` 失败：{error}", path.display()))
}

fn entry_name(entry: &std::fs::DirEntry) -> Result<String, String> {
    entry
        .file_name()
        .into_string()
        .map_err(|_| "发现非 UTF-8 文件名，无法证明历史为空".to_string())
}

fn path_exists_without_following(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("检查路径 `{}` 失败：{error}", path.display())),
    }
}

fn create_or_verify_durable_backup(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("角色备份路径缺少父目录"))?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(std::io::Error::other("既有角色备份不是普通文件"));
            }
            let existing = std::fs::read(path)?;
            if existing != content {
                return Err(std::io::Error::other("既有角色备份与当前源文件不一致"));
            }
            std::fs::File::open(path)?.sync_all()?;
            return sync_directory_strict(parent);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut backup = options.open(path)?;
    let result = (|| {
        backup.write_all(content)?;
        backup.sync_all()?;
        drop(backup);
        sync_directory_strict(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
        let _ = sync_directory_strict(parent);
    }
    result
}

#[cfg(unix)]
fn sync_directory_strict(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory_strict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_core::domain::usage::{ProviderTokenUsage, RuntimeTokenUsage, RuntimeUsageStore};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let suffix = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "muse-default-persona-migration-{prefix}-{nanos}-{suffix}"
        ));
        std::fs::create_dir_all(&path).expect("应能创建迁移测试目录");
        path
    }

    fn write_legacy_default_persona(data_dir: &Path) -> Vec<u8> {
        let persona_dir = data_dir.join("personas");
        std::fs::create_dir_all(&persona_dir).expect("应能创建角色目录");
        let content = serde_json::to_vec_pretty(&raw_default_store_value())
            .expect("应能序列化 v0.1.0 旧角色库");
        std::fs::write(persona_dir.join("personas.json"), &content)
            .expect("应能写入 v0.1.0 旧角色库");
        content
    }

    fn raw_default_store_value() -> serde_json::Value {
        let mut persona =
            serde_json::to_value(historical_default_persona()).expect("应能序列化历史默认角色");
        let persona = persona.as_object_mut().expect("历史默认角色应为对象");
        persona.remove("skill_policy");
        persona.remove("mcp_policy");
        serde_json::json!({
            "personas": [persona],
            "active_persona_id": LEGACY_DEFAULT_PERSONA_ID
        })
    }

    #[test]
    fn raw_template_match_rejects_missing_unknown_and_multi_persona_shapes() {
        let exact = serde_json::to_vec(&raw_default_store_value()).expect("应能序列化固定模板");
        assert!(raw_store_matches_historical_template(&exact));

        let mut missing = raw_default_store_value();
        missing["personas"][0]
            .as_object_mut()
            .expect("角色应为对象")
            .remove("notes");
        assert!(!raw_store_matches_historical_template(
            &serde_json::to_vec(&missing).expect("应能序列化缺字段模板")
        ));

        let mut unknown = raw_default_store_value();
        unknown["personas"][0]["origin"] = serde_json::json!("system");
        assert!(!raw_store_matches_historical_template(
            &serde_json::to_vec(&unknown).expect("应能序列化未知字段模板")
        ));

        let mut multiple = raw_default_store_value();
        let duplicate = multiple["personas"][0].clone();
        multiple["personas"]
            .as_array_mut()
            .expect("角色列表应为数组")
            .push(duplicate);
        assert!(!raw_store_matches_historical_template(
            &serde_json::to_vec(&multiple).expect("应能序列化多角色模板")
        ));
    }

    #[tokio::test]
    async fn removes_only_pristine_default_when_all_history_sources_are_empty() {
        let data_dir = unique_temp_dir("empty-history");
        let original = write_legacy_default_persona(&data_dir);
        let (session_store, migration) = SessionStore::open(&data_dir)
            .await
            .expect("应能打开空 canonical 会话存储");

        let outcome = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("严格空历史迁移应成功");

        let LegacyDefaultPersonaMigrationOutcome::Removed { backup_path } = outcome else {
            panic!("严格空历史应移除旧默认角色")
        };
        assert_eq!(std::fs::read(&backup_path).expect("备份应可读"), original);
        let reloaded = PersonaStore::load_from_dir(&data_dir).expect("应能读取迁移后角色库");
        assert!(reloaded.personas().is_empty());
        assert!(reloaded.active_persona_id().is_none());

        let second = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("重复迁移应幂等");
        assert_eq!(
            second,
            LegacyDefaultPersonaMigrationOutcome::AlreadyCompleted
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn completed_marker_preserves_manually_restored_default_persona() {
        let data_dir = unique_temp_dir("completed-restoration");
        let original = write_legacy_default_persona(&data_dir);
        let (session_store, migration) = SessionStore::open(&data_dir)
            .await
            .expect("应能打开空 canonical 会话存储");
        let first = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("首次迁移应成功");
        assert!(matches!(
            first,
            LegacyDefaultPersonaMigrationOutcome::Removed { .. }
        ));

        std::fs::write(data_dir.join("personas/personas.json"), original)
            .expect("应能模拟用户恢复旧角色文件");
        let restored = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("完成标记应阻止再次删除");

        assert_eq!(
            restored,
            LegacyDefaultPersonaMigrationOutcome::AlreadyCompleted
        );
        let reloaded = PersonaStore::load_from_dir(&data_dir).expect("应能读取恢复后的角色库");
        assert!(reloaded.get(LEGACY_DEFAULT_PERSONA_ID).is_some());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn preserves_pristine_default_when_canonical_history_exists() {
        let data_dir = unique_temp_dir("canonical-history");
        let original = write_legacy_default_persona(&data_dir);
        let (session_store, migration) = SessionStore::open(&data_dir)
            .await
            .expect("应能打开 canonical 会话存储");
        session_store
            .append_event(
                "default",
                Some("turn-history".to_string()),
                "user",
                serde_json::json!({
                    "conversation_id": "default",
                    "turn_id": "turn-history",
                    "persona_id": LEGACY_DEFAULT_PERSONA_ID,
                    "content": "已有用户内容"
                }),
            )
            .await
            .expect("应能写入 canonical 历史");

        let outcome = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("有历史时应保守返回");

        assert_eq!(
            outcome,
            LegacyDefaultPersonaMigrationOutcome::PreservedHistory
        );
        assert_eq!(
            std::fs::read(data_dir.join("personas/personas.json")).expect("角色源应保留"),
            original
        );
        assert!(
            !data_dir
                .join("personas")
                .join(LEGACY_DEFAULT_PERSONA_BACKUP_FILE)
                .exists()
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn preserves_pristine_default_when_runtime_usage_exists() {
        let data_dir = unique_temp_dir("runtime-usage");
        let original = write_legacy_default_persona(&data_dir);
        let usage_store =
            RuntimeUsageStore::load_from_dir(&data_dir).expect("应能创建测试运行时用量库");
        let usage = RuntimeTokenUsage::from_provider_usage(
            "usage-1",
            "default",
            "turn-1",
            "mock",
            "mock-model",
            "2026-07-12T00:00:00Z",
            ProviderTokenUsage::local_estimated(1, 1),
        );
        usage_store
            .record_token_usage(&usage)
            .expect("应能写入测试用量证据");
        let (session_store, migration) = SessionStore::open(&data_dir)
            .await
            .expect("应能打开空 canonical 会话存储");

        let outcome = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("存在用量时应保守返回");

        assert_eq!(
            outcome,
            LegacyDefaultPersonaMigrationOutcome::PreservedHistory
        );
        assert_eq!(
            std::fs::read(data_dir.join("personas/personas.json")).expect("角色源应保留"),
            original
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn preserves_pristine_default_when_chat_request_evidence_exists() {
        let data_dir = unique_temp_dir("chat-request-evidence");
        let original = write_legacy_default_persona(&data_dir);
        let (session_store, migration) = SessionStore::open(&data_dir)
            .await
            .expect("应能打开空 canonical 会话存储");
        let request_dir = data_dir.join("runtime/chat-requests");
        std::fs::create_dir_all(&request_dir).expect("应能创建请求证据目录");
        std::fs::write(request_dir.join("request.jsonl"), b"accepted\n").expect("应能写入请求证据");

        let outcome = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect("存在请求证据时应保守返回");

        assert!(matches!(
            outcome,
            LegacyDefaultPersonaMigrationOutcome::PreservedUncertain { .. }
        ));
        assert_eq!(
            std::fs::read(data_dir.join("personas/personas.json")).expect("角色源应保留"),
            original
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn backup_mismatch_preserves_source_bytes() {
        let data_dir = unique_temp_dir("backup-mismatch");
        let original = write_legacy_default_persona(&data_dir);
        let backup_path = data_dir
            .join("personas")
            .join(LEGACY_DEFAULT_PERSONA_BACKUP_FILE);
        std::fs::write(&backup_path, b"conflicting backup").expect("应能写入冲突备份");
        let (session_store, migration) = SessionStore::open(&data_dir)
            .await
            .expect("应能打开空 canonical 会话存储");

        let error = migrate_pristine_legacy_default_persona(
            &data_dir,
            &session_store,
            migration.quarantined_records,
        )
        .await
        .expect_err("冲突备份必须阻止迁移");

        assert!(error.contains("既有角色备份与当前源文件不一致"));
        assert_eq!(
            std::fs::read(data_dir.join("personas/personas.json")).expect("角色源应保留"),
            original
        );
        let reloaded = PersonaStore::load_from_dir(&data_dir).expect("应能读取保留角色库");
        assert!(reloaded.get(LEGACY_DEFAULT_PERSONA_ID).is_some());
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
