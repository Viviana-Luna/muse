use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Persona, PersonaSummary, PersonaValidationError};

const PERSONA_STORE_DIR: &str = "personas";
const PERSONA_STORE_FILE: &str = "personas.json";

/// 本地角色存储读写错误。
#[derive(Debug)]
pub enum PersonaStoreError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Validation(PersonaValidationError),
    DuplicateId(String),
    ActivePersonaMissing(String),
    PersonaNotFound(String),
}

impl std::fmt::Display for PersonaStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersonaStoreError::Io(err) => write!(f, "角色存储读写失败：{err}"),
            PersonaStoreError::Serde(err) => write!(f, "角色存储序列化失败：{err}"),
            PersonaStoreError::Validation(err) => write!(f, "角色校验失败：{err}"),
            PersonaStoreError::DuplicateId(id) => write!(f, "发现重复角色 id：{id}"),
            PersonaStoreError::ActivePersonaMissing(id) => {
                write!(f, "激活角色 `{id}` 在本地存储中不存在")
            }
            PersonaStoreError::PersonaNotFound(id) => write!(f, "角色 `{id}` 不存在"),
        }
    }
}

impl std::error::Error for PersonaStoreError {}

impl From<std::io::Error> for PersonaStoreError {
    fn from(value: std::io::Error) -> Self {
        PersonaStoreError::Io(value)
    }
}

impl From<serde_json::Error> for PersonaStoreError {
    fn from(value: serde_json::Error) -> Self {
        PersonaStoreError::Serde(value)
    }
}

impl From<PersonaValidationError> for PersonaStoreError {
    fn from(value: PersonaValidationError) -> Self {
        PersonaStoreError::Validation(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedPersonaStore {
    #[serde(default)]
    personas: Vec<Persona>,
    #[serde(default)]
    active_persona_id: Option<String>,
}

/// 角色本地存储骨架。
#[derive(Debug, Clone)]
pub struct PersonaStore {
    storage_path: PathBuf,
    personas: Vec<Persona>,
    active_persona_id: Option<String>,
}

impl PersonaStore {
    /// 从指定基础目录加载角色存储；若文件不存在，则返回空存储。
    pub fn load_from_dir(base_dir: impl AsRef<Path>) -> Result<Self, PersonaStoreError> {
        let storage_path = Self::storage_path(base_dir.as_ref());

        if !storage_path.exists() {
            return Ok(Self {
                storage_path,
                personas: Vec::new(),
                active_persona_id: None,
            });
        }

        let content = std::fs::read_to_string(&storage_path)?;
        let normalized = normalize_json_text(&content);
        if normalized.trim().is_empty() {
            return Ok(Self {
                storage_path,
                personas: Vec::new(),
                active_persona_id: None,
            });
        }
        let persisted: PersistedPersonaStore = serde_json::from_str(normalized)?;
        Self::from_persisted(storage_path, persisted)
    }

    /// 以 JSON 文件形式持久化当前角色存储。
    pub fn save(&self) -> Result<(), PersonaStoreError> {
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let persisted = PersistedPersonaStore {
            personas: self.personas.clone(),
            active_persona_id: self.active_persona_id.clone(),
        };
        let content = serde_json::to_string_pretty(&persisted)?;
        crate::model::config::store::atomic_write_synced(&self.storage_path, content.as_bytes())?;
        Ok(())
    }

    pub fn list(&self) -> Vec<PersonaSummary> {
        self.personas.iter().map(Persona::summary).collect()
    }

    pub fn personas(&self) -> &[Persona] {
        &self.personas
    }

    pub fn active_persona_id(&self) -> Option<&str> {
        self.active_persona_id.as_deref()
    }

    pub fn active_persona(&self) -> Option<&Persona> {
        self.active_persona_id
            .as_deref()
            .and_then(|id| self.get(id))
    }

    pub fn get(&self, id: &str) -> Option<&Persona> {
        self.personas.iter().find(|persona| persona.id == id)
    }

    /// 创建新角色；若 id 已存在则返回错误。
    pub fn create(&mut self, persona: Persona) -> Result<(), PersonaStoreError> {
        persona.validate()?;

        if self.get(&persona.id).is_some() {
            return Err(PersonaStoreError::DuplicateId(persona.id));
        }

        self.personas.push(persona);
        Ok(())
    }

    /// 插入新角色或覆盖同 id 角色。
    #[allow(dead_code)]
    pub fn upsert(&mut self, persona: Persona) -> Result<(), PersonaStoreError> {
        persona.validate()?;

        if let Some(existing) = self.personas.iter_mut().find(|item| item.id == persona.id) {
            *existing = persona;
            return Ok(());
        }

        self.personas.push(persona);
        Ok(())
    }

    /// 更新已存在角色；若不存在则返回错误。
    pub fn update(&mut self, persona: Persona) -> Result<(), PersonaStoreError> {
        persona.validate()?;

        let Some(existing) = self.personas.iter_mut().find(|item| item.id == persona.id) else {
            return Err(PersonaStoreError::PersonaNotFound(persona.id));
        };

        *existing = persona;
        Ok(())
    }

    /// 删除指定角色；若删除的是激活角色，会自动清空激活状态。
    pub fn delete(&mut self, id: &str) -> bool {
        let original_len = self.personas.len();
        self.personas.retain(|persona| persona.id != id);

        if self.active_persona_id.as_deref() == Some(id) {
            self.active_persona_id = None;
        }

        self.personas.len() != original_len
    }

    pub fn set_active(&mut self, id: &str) -> Result<(), PersonaStoreError> {
        if self.get(id).is_none() {
            return Err(PersonaStoreError::PersonaNotFound(id.to_string()));
        }

        self.active_persona_id = Some(id.to_string());
        Ok(())
    }

    fn storage_path(base_dir: &Path) -> PathBuf {
        base_dir.join(PERSONA_STORE_DIR).join(PERSONA_STORE_FILE)
    }

    fn from_persisted(
        storage_path: PathBuf,
        mut persisted: PersistedPersonaStore,
    ) -> Result<Self, PersonaStoreError> {
        let mut seen_ids = HashSet::new();
        for persona in &mut persisted.personas {
            persona.migrate_legacy_defaults();
            persona.validate()?;
            if !seen_ids.insert(persona.id.clone()) {
                return Err(PersonaStoreError::DuplicateId(persona.id.clone()));
            }
        }

        if let Some(active_id) = persisted.active_persona_id.as_deref()
            && !persisted
                .personas
                .iter()
                .any(|persona| persona.id == active_id)
        {
            return Err(PersonaStoreError::ActivePersonaMissing(
                active_id.to_string(),
            ));
        }

        Ok(Self {
            storage_path,
            personas: persisted.personas,
            active_persona_id: persisted.active_persona_id,
        })
    }
}

fn normalize_json_text(content: &str) -> &str {
    content.trim_start_matches('\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::{PersonaStore, PersonaStoreError};
    use crate::domain::persona::{Persona, RoleplayStyle, ToolPolicy};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("muse-persona-store-{nanos}-{counter}"))
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
            default_visual_pack_id: "room-default".to_string(),
            author: "rainy".to_string(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn loads_empty_store_when_file_missing() {
        let dir = unique_temp_dir();
        let store = PersonaStore::load_from_dir(&dir).expect("应返回空存储");

        assert!(store.personas().is_empty());
        assert!(store.active_persona_id().is_none());
    }

    #[test]
    fn saves_and_reloads_personas() {
        let dir = unique_temp_dir();
        let mut store = PersonaStore::load_from_dir(&dir).expect("首次加载失败");

        store
            .upsert(sample_persona("persona-a"))
            .expect("写入角色失败");
        store.set_active("persona-a").expect("激活角色失败");
        store.save().expect("保存角色失败");

        let reloaded = PersonaStore::load_from_dir(&dir).expect("重载角色失败");
        assert_eq!(reloaded.personas().len(), 1);
        assert_eq!(reloaded.active_persona_id(), Some("persona-a"));
        assert_eq!(reloaded.list()[0].id, "persona-a");
    }

    #[test]
    fn clears_active_persona_when_deleted() {
        let dir = unique_temp_dir();
        let mut store = PersonaStore::load_from_dir(&dir).expect("首次加载失败");

        store
            .upsert(sample_persona("persona-b"))
            .expect("写入角色失败");
        store.set_active("persona-b").expect("激活角色失败");
        assert!(store.delete("persona-b"));
        assert!(store.active_persona_id().is_none());
    }

    #[test]
    fn create_rejects_duplicate_id() {
        let dir = unique_temp_dir();
        let mut store = PersonaStore::load_from_dir(&dir).expect("首次加载失败");

        store
            .create(sample_persona("persona-c"))
            .expect("首次创建角色失败");

        let err = store
            .create(sample_persona("persona-c"))
            .expect_err("重复 id 应返回错误");
        assert!(matches!(err, PersonaStoreError::DuplicateId(id) if id == "persona-c"));
    }

    #[test]
    fn update_requires_existing_persona() {
        let dir = unique_temp_dir();
        let mut store = PersonaStore::load_from_dir(&dir).expect("首次加载失败");

        let err = store
            .update(sample_persona("missing-persona"))
            .expect_err("更新不存在的角色应失败");
        assert!(matches!(err, PersonaStoreError::PersonaNotFound(id) if id == "missing-persona"));
    }

    #[test]
    fn loads_store_with_utf8_bom() {
        let dir = unique_temp_dir();
        let storage_dir = dir.join("personas");
        std::fs::create_dir_all(&storage_dir).expect("创建角色目录失败");
        let storage_path = storage_dir.join("personas.json");
        let json = "{\n  \"personas\": [],\n  \"active_persona_id\": null\n}";
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(json.as_bytes());
        std::fs::write(&storage_path, bytes).expect("写入带 BOM 的角色存储失败");

        let store = PersonaStore::load_from_dir(&dir).expect("应能兼容带 BOM 的角色存储");
        assert!(store.personas().is_empty());
        assert!(store.active_persona_id().is_none());
    }

    #[test]
    fn loads_legacy_persona_without_new_required_fields() {
        let dir = unique_temp_dir();
        let storage_dir = dir.join("personas");
        std::fs::create_dir_all(&storage_dir).expect("创建角色目录失败");
        let storage_path = storage_dir.join("personas.json");
        let json = r#"{
          "personas": [{
            "id": "legacy-persona",
            "name": "旧角色",
            "system_prompt": "保持角色一致。",
            "default_visual_pack_id": "default-visual-pack"
          }],
          "active_persona_id": "legacy-persona"
        }"#;
        std::fs::write(&storage_path, json).expect("写入旧角色存储失败");

        let store = PersonaStore::load_from_dir(&dir).expect("应能加载旧角色存储");
        let persona = store.get("legacy-persona").expect("应保留旧角色");
        assert!(!persona.character_profile.trim().is_empty());
        assert_eq!(persona.world_profile, "现实日常");
    }
}
