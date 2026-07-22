//! 应用偏好接口的请求、响应与错误 DTO。

use serde::{Deserialize, Serialize};

/// 外观偏好更新请求。
#[derive(Deserialize)]
pub(crate) struct AppearancePreferencesUpdateRequest {
    pub(crate) background_blur: u8,
    pub(crate) background_opacity: f64,
    pub(crate) motion_level: muse_core::app::preferences::MotionLevel,
}

/// 外观偏好与配置诊断响应。
#[derive(Serialize)]
pub(crate) struct AppearancePreferencesResponse {
    schema_version: u32,
    appearance: muse_core::app::preferences::AppearancePreferences,
    diagnostics: Vec<muse_core::app::preferences::ConfigDiagnostic>,
}

/// 用户配置变更失败响应。
#[derive(Serialize)]
pub(crate) struct ConfigMutationErrorResponse {
    pub(crate) error: String,
    pub(crate) code: String,
    pub(crate) field_path: String,
    pub(crate) message: String,
}

impl From<muse_core::app::preferences::MuseConfigSnapshot> for AppearancePreferencesResponse {
    fn from(snapshot: muse_core::app::preferences::MuseConfigSnapshot) -> Self {
        Self {
            schema_version: snapshot.config.schema_version,
            appearance: snapshot.config.appearance,
            diagnostics: snapshot.diagnostics,
        }
    }
}
