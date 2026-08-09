//! 应用偏好接口的请求、响应与错误 DTO。

use muse_core::app::preferences::WebSearchProvider;
use serde::{Deserialize, Serialize};

/// 外观偏好更新请求。
#[derive(Deserialize)]
pub(crate) struct AppearancePreferencesUpdateRequest {
    #[serde(default)]
    pub(crate) theme: Option<String>,
    #[serde(default)]
    pub(crate) background_theme: Option<String>,
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

#[cfg(test)]
mod appearance_preferences_tests {
    use super::AppearancePreferencesUpdateRequest;

    #[test]
    fn appearance_update_accepts_existing_theme_field() {
        let request: AppearancePreferencesUpdateRequest = serde_json::from_str(
            r#"{"theme":"light","background_theme":"light","background_blur":18,"background_opacity":1.0,"motion_level":"full"}"#,
        )
        .expect("外观更新应接受既有主题字段");

        assert_eq!(request.theme.as_deref(), Some("light"));
        assert_eq!(request.background_theme.as_deref(), Some("light"));
    }

    #[test]
    fn appearance_update_keeps_legacy_clients_compatible() {
        let request: AppearancePreferencesUpdateRequest = serde_json::from_str(
            r#"{"background_blur":18,"background_opacity":1.0,"motion_level":"full"}"#,
        )
        .expect("旧外观更新请求应继续可解析");

        assert!(request.theme.is_none());
        assert!(request.background_theme.is_none());
    }
}

/// `GET /api/web-search/config` 响应：联网搜索后端与凭据配置状态。
/// 仅返回是否已配置，绝不返回 Exa API Key 或其掩码。
#[derive(Debug, Serialize)]
pub struct WebSearchConfigResponse {
    pub provider: WebSearchProvider,
    pub api_key_configured: bool,
}

/// 密钥变更动作。只有 `replace` 允许同时提交 `value`。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SecretUpdateAction {
    /// 保留当前存储中的值。
    #[default]
    Keep,
    /// 使用请求中的 `value` 替换当前值。
    Replace,
    /// 删除当前存储中的值。
    Delete,
}

/// 统一密钥更新契约，响应体永不回显 `value`。
#[derive(Debug, Default, Deserialize)]
pub struct SecretUpdate {
    #[serde(default)]
    pub action: SecretUpdateAction,
    #[serde(default)]
    pub value: Option<String>,
}

/// `PUT /api/web-search/config` 请求体：非敏感后端选择与密钥动作一次提交。
#[derive(Debug, Default, Deserialize)]
pub struct WebSearchConfigUpdate {
    #[serde(default)]
    pub provider: WebSearchProvider,
    #[serde(default)]
    pub action: SecretUpdateAction,
    #[serde(default)]
    pub value: Option<String>,
}

#[cfg(test)]
mod web_search_config_tests {
    use super::{SecretUpdateAction, WebSearchConfigResponse, WebSearchConfigUpdate};
    use muse_core::app::preferences::WebSearchProvider;

    #[test]
    fn web_search_config_response_never_contains_secret_value() {
        let response = WebSearchConfigResponse {
            provider: WebSearchProvider::ExaApi,
            api_key_configured: true,
        };
        let json = serde_json::to_value(response).expect("配置状态应能序列化");

        assert_eq!(json["provider"], "exa_api");
        assert_eq!(json["api_key_configured"], true);
        assert!(json.get("api_key").is_none());
    }

    #[test]
    fn web_search_config_update_accepts_explicit_replace() {
        let request: WebSearchConfigUpdate = serde_json::from_str(
            r#"{"provider":"exa_api","action":"replace","value":"exa-secret-value"}"#,
        )
        .expect("请求体应能解析");

        assert_eq!(request.provider, WebSearchProvider::ExaApi);
        assert_eq!(request.action, SecretUpdateAction::Replace);
        assert_eq!(request.value.as_deref(), Some("exa-secret-value"));
    }

    #[test]
    fn web_search_config_update_defaults_to_keep() {
        let request: WebSearchConfigUpdate = serde_json::from_str(r#"{}"#).expect("请求体应能解析");

        assert_eq!(request.provider, WebSearchProvider::ExaFreeMcp);
        assert_eq!(request.action, SecretUpdateAction::Keep);
        assert!(request.value.is_none());
    }
}

/// `GET /api/models/config` 响应：只暴露密钥是否存在，不返回明文或掩码。
#[derive(Serialize)]
pub struct ModelsConfigResponse {
    pub chat: ConfigSectionResponse,
    pub tts: TtsConfigResponse,
    pub speech_recognition: SpeechRecognitionConfigResponse,
    pub audio_understanding: ConfigSectionResponse,
    pub voice_input: VoiceInputConfigResponse,
}

/// 单段模型配置响应体。
#[derive(Serialize)]
pub struct ConfigSectionResponse {
    pub provider: String,
    pub api_base: String,
    pub api_protocol: String,
    pub api_key_configured: bool,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub voice_id: Option<String>,
    pub speed: Option<f32>,
}

/// 语音合成配置响应体。
#[derive(Serialize)]
pub struct TtsConfigResponse {
    pub enabled: bool,
    pub provider: String,
    pub api_base: String,
    pub api_key_configured: bool,
    pub model: String,
    pub voice_id: String,
    pub speed: f32,
    pub response_format: String,
}

#[cfg(test)]
mod model_secret_response_tests {
    use super::ConfigSectionResponse;

    #[test]
    fn model_config_response_only_exposes_configured_state() {
        let response = ConfigSectionResponse {
            provider: "openai".to_string(),
            api_base: "https://api.example.com/v1".to_string(),
            api_protocol: "chat_completions".to_string(),
            api_key_configured: true,
            model: "example".to_string(),
            max_tokens: Some(1024),
            temperature: Some(0.2),
            voice_id: None,
            speed: None,
        };
        let value = serde_json::to_value(response).expect("模型配置响应应能序列化");

        assert_eq!(value["api_key_configured"], true);
        assert!(value.get("api_key").is_none());
    }
}

/// 语音输入策略响应体。
#[derive(Serialize)]
pub struct VoiceInputConfigResponse {
    pub mode: String,
}

/// OpenAI-compatible 语音识别配置响应体。
#[derive(Serialize)]
pub struct SpeechRecognitionConfigResponse {
    pub enabled: bool,
    pub provider: String,
    pub api_base: String,
    pub api_key_configured: bool,
    pub model: String,
    pub language: String,
    pub response_format: String,
}

/// `PUT /api/models/config` 请求体；`api_key` 为空表示保持原值。
#[derive(Deserialize)]
pub struct ModelsConfigUpdate {
    pub chat: ConfigSectionUpdate,
    pub tts: TtsConfigUpdate,
    #[serde(default)]
    pub speech_recognition: SpeechRecognitionConfigUpdate,
    #[serde(default)]
    pub audio_understanding: ConfigSectionUpdate,
    #[serde(default)]
    pub voice_input: VoiceInputConfigUpdate,
}

/// 单段模型配置更新请求体。
#[derive(Default, Deserialize)]
pub struct ConfigSectionUpdate {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_protocol: String,
    /// 空值或缺省表示保持原值；非空字符串表示更新。
    #[serde(default)]
    pub api_key: Option<String>,
    /// v1 统一密钥动作；存在时优先于旧 `api_key` 字段。
    #[serde(default)]
    pub api_key_update: SecretUpdate,
    #[serde(default)]
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub voice_id: Option<String>,
    pub speed: Option<f32>,
}

/// 语音合成配置更新请求体。
#[derive(Default, Deserialize)]
pub struct TtsConfigUpdate {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    /// 空值或缺省表示保持原值；非空字符串表示更新。
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_update: SecretUpdate,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub voice_id: String,
    pub speed: Option<f32>,
    #[serde(default)]
    pub response_format: String,
}

/// 语音输入策略更新请求体。
#[derive(Default, Deserialize)]
pub struct VoiceInputConfigUpdate {
    #[serde(default)]
    pub mode: String,
}

/// OpenAI-compatible 语音识别配置更新请求体。
#[derive(Default, Deserialize)]
pub struct SpeechRecognitionConfigUpdate {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_update: SecretUpdate,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub response_format: String,
}
