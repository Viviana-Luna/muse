//! 语音领域抽象模块，统一 OpenAI-compatible 语音合成、语音识别和错误分类。

pub mod factory;
pub mod openai_asr;
pub mod openai_tts;

use async_trait::async_trait;

/// 语音合成与语音识别共用错误类型。
///
/// 语义与 `ChatModelError` 对齐：接口调用失败、网络失败、配置缺失分别归位，
/// 便于上层统一转换为 HTTP 响应与前端可读提示。
#[derive(Debug)]
pub enum VoiceError {
    /// 语音提供器返回了非成功状态或无法解析的响应体。
    ApiError(String),
    /// 与语音服务通信时出现网络层错误。
    NetworkError(String),
    /// 语音段未启用或缺少必要配置（如 `api_key`）。
    ConfigError(String),
    /// 音频数据无法解码或编码失败。
    DecodeError(String),
    /// 录音中没有检测到足够的人声。
    NoSpeech(String),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VoiceError::ApiError(msg) => write!(f, "语音服务调用失败：{msg}"),
            VoiceError::NetworkError(msg) => write!(f, "语音服务网络错误：{msg}"),
            VoiceError::ConfigError(msg) => write!(f, "语音服务配置错误：{msg}"),
            VoiceError::DecodeError(msg) => write!(f, "音频数据解析失败：{msg}"),
            VoiceError::NoSpeech(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for VoiceError {}

/// 语音合成结果。
#[derive(Debug, Clone)]
pub struct SynthesizedAudio {
    /// 音频字节数据。
    pub bytes: Vec<u8>,
    /// 网页接口返回给前端的音频媒体类型。
    pub content_type: String,
}

/// 语音合成结果：返回音频字节与媒体类型。
pub type TtsResult = Result<SynthesizedAudio, VoiceError>;

/// 语音识别结果：返回转写后的文本。
pub type SpeechRecognitionResult = Result<String, VoiceError>;

/// 语音合成提供器抽象。
///
/// 各家语音合成服务实现此接口，上层路由只感知"文本进、音频出"。
/// 当前正式实现仅支持 OpenAI-compatible Speech API。
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// 把文本合成为音频，返回音频字节与媒体类型。
    async fn synthesize(&self, text: &str) -> TtsResult;
    /// 返回提供器名称，用于诊断与前端展示。
    /// 当前路由层未暴露，保留给后续诊断接口或日志。
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}

/// OpenAI-compatible 语音识别抽象。
#[async_trait]
pub trait SpeechRecognitionProvider: Send + Sync {
    /// 把音频字节转写为文本。
    async fn transcribe(&self, audio: Vec<u8>, audio_format: &str) -> SpeechRecognitionResult;
    /// 返回提供器名称，用于诊断与前端展示。
    /// 当前路由层未暴露，保留给后续诊断接口或日志。
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}
