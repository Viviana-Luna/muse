pub mod store;

use serde::{Deserialize, Serialize};

/// 角色展示包主体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualPack {
    pub id: String,
    pub name: String,
    pub portrait_path: String,
    pub background_path: String,
    #[serde(default)]
    pub avatar_path: String,
    #[serde(default)]
    pub theme_color: String,
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
    #[serde(default)]
    pub layout_mode: String,
    #[serde(default = "default_portrait_frame")]
    pub portrait_frame: String,
    #[serde(default = "default_portrait_fit")]
    pub portrait_fit: String,
    #[serde(default = "default_portrait_position")]
    pub portrait_position_x: i32,
    #[serde(default = "default_portrait_position")]
    pub portrait_position_y: i32,
    #[serde(default = "default_portrait_scale")]
    pub portrait_scale: u16,
    #[serde(default)]
    pub fallback_text: String,
    #[serde(default = "default_visual_pack_version")]
    pub version: String,
    #[serde(default)]
    pub notes: String,
}

/// 展示包字段校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualPackValidationError {
    EmptyField(&'static str),
    InvalidId,
}

impl std::fmt::Display for VisualPackValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisualPackValidationError::EmptyField(field) => {
                write!(f, "展示包字段 `{field}` 不能为空")
            }
            VisualPackValidationError::InvalidId => {
                write!(f, "展示包 `id` 仅允许字母、数字、短横线和下划线")
            }
        }
    }
}

impl std::error::Error for VisualPackValidationError {}

impl VisualPack {
    /// 校验当前展示包的最小可用字段。
    pub fn validate(&self) -> Result<(), VisualPackValidationError> {
        if self.id.trim().is_empty() {
            return Err(VisualPackValidationError::EmptyField("id"));
        }
        if !self
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(VisualPackValidationError::InvalidId);
        }
        if self.name.trim().is_empty() {
            return Err(VisualPackValidationError::EmptyField("name"));
        }
        Ok(())
    }
}

fn default_visual_pack_version() -> String {
    "1.0.0".to_string()
}

fn default_theme_mode() -> String {
    "auto".to_string()
}

fn default_portrait_frame() -> String {
    "portrait".to_string()
}

fn default_portrait_fit() -> String {
    "cover".to_string()
}

fn default_portrait_position() -> i32 {
    50
}

fn default_portrait_scale() -> u16 {
    100
}

#[cfg(test)]
mod tests {
    use super::{VisualPack, VisualPackValidationError};

    fn valid_visual_pack() -> VisualPack {
        VisualPack {
            id: "default-room".to_string(),
            name: "默认展示包".to_string(),
            portrait_path: "/assets/test-character.png".to_string(),
            background_path: "/assets/test-background.png".to_string(),
            avatar_path: String::new(),
            theme_color: "#d8596f".to_string(),
            theme_mode: "auto".to_string(),
            layout_mode: "portrait-right".to_string(),
            portrait_frame: "portrait".to_string(),
            portrait_fit: "cover".to_string(),
            portrait_position_x: 50,
            portrait_position_y: 50,
            portrait_scale: 100,
            fallback_text: "资源缺失，请导入展示包。".to_string(),
            version: "1.0.0".to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn validates_required_fields() {
        let visual_pack = valid_visual_pack();
        assert!(visual_pack.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_id() {
        let mut visual_pack = valid_visual_pack();
        visual_pack.id = "bad id".to_string();

        assert_eq!(
            visual_pack.validate(),
            Err(VisualPackValidationError::InvalidId)
        );
    }

    #[test]
    fn allows_visual_pack_without_images() {
        let mut visual_pack = valid_visual_pack();
        visual_pack.portrait_path.clear();
        visual_pack.background_path = " ".to_string();
        visual_pack.avatar_path.clear();

        assert!(visual_pack.validate().is_ok());
    }

    #[test]
    fn fills_portrait_composition_defaults_for_legacy_json() {
        let visual_pack: VisualPack = serde_json::from_str(
            r##"{
              "id": "legacy-pack",
              "name": "旧展示包",
              "portrait_path": "/assets/legacy-character.png",
              "background_path": "/assets/legacy-background.png",
              "theme_color": "#d8596f",
              "layout_mode": "portrait-right"
            }"##,
        )
        .expect("旧展示包 JSON 应能兼容新增构图字段");

        assert_eq!(visual_pack.portrait_frame, "portrait");
        assert_eq!(visual_pack.theme_mode, "auto");
        assert_eq!(visual_pack.portrait_fit, "cover");
        assert_eq!(visual_pack.portrait_position_x, 50);
        assert_eq!(visual_pack.portrait_position_y, 50);
        assert_eq!(visual_pack.portrait_scale, 100);
    }
}
