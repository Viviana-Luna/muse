use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{VisualPack, VisualPackValidationError};

const VISUAL_PACK_STORE_DIR: &str = "visual_packs";
const VISUAL_PACK_STORE_FILE: &str = "visual_packs.json";

/// 本地展示包存储读写错误。
#[derive(Debug)]
pub enum VisualPackStoreError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Validation(VisualPackValidationError),
    DuplicateId(String),
}

impl std::fmt::Display for VisualPackStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisualPackStoreError::Io(err) => write!(f, "展示包存储读写失败：{err}"),
            VisualPackStoreError::Serde(err) => write!(f, "展示包存储序列化失败：{err}"),
            VisualPackStoreError::Validation(err) => write!(f, "展示包校验失败：{err}"),
            VisualPackStoreError::DuplicateId(id) => write!(f, "发现重复展示包 id：{id}"),
        }
    }
}

impl std::error::Error for VisualPackStoreError {}

impl From<std::io::Error> for VisualPackStoreError {
    fn from(value: std::io::Error) -> Self {
        VisualPackStoreError::Io(value)
    }
}

impl From<serde_json::Error> for VisualPackStoreError {
    fn from(value: serde_json::Error) -> Self {
        VisualPackStoreError::Serde(value)
    }
}

impl From<VisualPackValidationError> for VisualPackStoreError {
    fn from(value: VisualPackValidationError) -> Self {
        VisualPackStoreError::Validation(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedVisualPackStore {
    #[serde(default)]
    visual_packs: Vec<VisualPack>,
}

/// 展示包本地存储骨架。
#[derive(Debug, Clone, Default)]
pub struct VisualPackStore {
    storage_path: PathBuf,
    visual_packs: Vec<VisualPack>,
}

impl VisualPackStore {
    /// 从指定基础目录加载展示包存储；若文件不存在，则返回空存储。
    pub fn load_from_dir(base_dir: impl AsRef<Path>) -> Result<Self, VisualPackStoreError> {
        let storage_path = Self::storage_path(base_dir.as_ref());

        if !storage_path.exists() {
            return Ok(Self {
                storage_path,
                visual_packs: Vec::new(),
            });
        }

        let content = std::fs::read_to_string(&storage_path)?;
        let normalized = normalize_json_text(&content);
        if normalized.trim().is_empty() {
            return Ok(Self {
                storage_path,
                visual_packs: Vec::new(),
            });
        }
        let persisted: PersistedVisualPackStore = serde_json::from_str(normalized)?;
        Self::from_persisted(storage_path, persisted)
    }

    /// 持久化当前展示包存储。
    pub fn save(&self) -> Result<(), VisualPackStoreError> {
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let persisted = PersistedVisualPackStore {
            visual_packs: self.visual_packs.clone(),
        };
        let content = serde_json::to_string_pretty(&persisted)?;
        std::fs::write(&self.storage_path, content)?;
        Ok(())
    }

    pub fn visual_packs(&self) -> &[VisualPack] {
        &self.visual_packs
    }

    pub fn get(&self, id: &str) -> Option<&VisualPack> {
        self.visual_packs.iter().find(|pack| pack.id == id)
    }

    /// 插入展示包；若 id 已存在则覆盖。
    pub fn upsert(&mut self, visual_pack: VisualPack) -> Result<(), VisualPackStoreError> {
        visual_pack.validate()?;

        if let Some(existing) = self
            .visual_packs
            .iter_mut()
            .find(|pack| pack.id == visual_pack.id)
        {
            *existing = visual_pack;
            return Ok(());
        }

        self.visual_packs.push(visual_pack);
        Ok(())
    }

    /// 删除指定展示包；不存在时保持幂等。
    pub fn delete(&mut self, id: &str) -> bool {
        let previous_len = self.visual_packs.len();
        self.visual_packs.retain(|pack| pack.id != id);
        self.visual_packs.len() != previous_len
    }

    fn storage_path(base_dir: &Path) -> PathBuf {
        base_dir
            .join(VISUAL_PACK_STORE_DIR)
            .join(VISUAL_PACK_STORE_FILE)
    }

    fn from_persisted(
        storage_path: PathBuf,
        persisted: PersistedVisualPackStore,
    ) -> Result<Self, VisualPackStoreError> {
        let mut seen_ids = HashSet::new();
        for visual_pack in &persisted.visual_packs {
            visual_pack.validate()?;
            if !seen_ids.insert(visual_pack.id.clone()) {
                return Err(VisualPackStoreError::DuplicateId(visual_pack.id.clone()));
            }
        }

        Ok(Self {
            storage_path,
            visual_packs: persisted.visual_packs,
        })
    }
}

fn normalize_json_text(content: &str) -> &str {
    content.trim_start_matches('\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::VisualPackStore;
    use crate::domain::persona::visual::VisualPack;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        std::env::temp_dir().join(format!("muse-visual-pack-store-{suffix}"))
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
    fn loads_empty_store_when_file_missing() {
        let dir = unique_temp_dir();
        let store = VisualPackStore::load_from_dir(&dir).expect("应返回空存储");

        assert!(store.visual_packs().is_empty());
    }

    #[test]
    fn saves_and_reloads_visual_packs() {
        let dir = unique_temp_dir();
        let mut store = VisualPackStore::load_from_dir(&dir).expect("首次加载失败");

        store
            .upsert(sample_visual_pack("default-room"))
            .expect("写入展示包失败");
        store.save().expect("保存展示包失败");

        let reloaded = VisualPackStore::load_from_dir(&dir).expect("重载展示包失败");
        assert_eq!(reloaded.visual_packs().len(), 1);
        assert_eq!(
            reloaded.get("default-room").map(|pack| pack.name.as_str()),
            Some("默认展示包")
        );
    }

    #[test]
    fn loads_store_with_utf8_bom() {
        let dir = unique_temp_dir();
        let storage_dir = dir.join("visual_packs");
        std::fs::create_dir_all(&storage_dir).expect("创建展示包目录失败");
        let storage_path = storage_dir.join("visual_packs.json");
        let json = "{\n  \"visual_packs\": []\n}";
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(json.as_bytes());
        std::fs::write(&storage_path, bytes).expect("写入带 BOM 的展示包存储失败");

        let store = VisualPackStore::load_from_dir(&dir).expect("应能兼容带 BOM 的展示包存储");
        assert!(store.visual_packs().is_empty());
    }

    #[test]
    fn delete_is_idempotent_and_only_removes_target_pack() {
        let mut store = VisualPackStore::default();
        store
            .upsert(sample_visual_pack("target"))
            .expect("应能插入目标展示包");
        store
            .upsert(sample_visual_pack("kept"))
            .expect("应能插入保留展示包");

        assert!(store.delete("target"));
        assert!(!store.delete("target"));
        assert!(store.get("target").is_none());
        assert!(store.get("kept").is_some());
    }
}
