//! 语音接口的请求与响应 DTO。

use serde::{Deserialize, Serialize};

/// 文本转语音请求体。
#[derive(Deserialize)]
pub struct TtsRequest {
    pub text: String,
    #[serde(default)]
    pub voice_id: Option<String>,
}

/// 语音转文本响应体。
#[derive(Serialize)]
pub struct SpeechTranscriptionResponse {
    pub text: String,
}

/// 调试环境下扩展子服务探测响应体。
#[cfg(debug_assertions)]
#[derive(Serialize)]
pub struct DebugExpandServiceResponse {
    pub service: String,
    pub model_ready: bool,
    pub text: String,
}

/// 语音能力探测响应体。
#[derive(Serialize)]
pub struct VoiceCapabilitiesResponse {
    /// 语音合成是否可用（提供器已启用）。
    pub tts: bool,
    /// 语音识别服务是否可用。
    pub speech_recognition: bool,
}
