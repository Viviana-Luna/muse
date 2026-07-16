//! 语音能力工厂模块，根据运行时配置创建 OpenAI-compatible 语音提供器。

use super::openai_asr::OpenAiAudioTranscriptionProvider;
use super::openai_tts::OpenAiSpeechTtsProvider;
use super::{SpeechRecognitionProvider, TtsProvider, VoiceError};
use crate::model::config::{SpeechRecognitionConfig, TtsConfig};

/// 根据配置创建语音合成提供器。
///
/// 当语音合成段未启用（`enabled=false`）时返回空值，上层据此标记语音合成不可用。
pub fn create_tts_provider(config: &TtsConfig) -> Result<Option<Box<dyn TtsProvider>>, VoiceError> {
    if !config.enabled() {
        return Ok(None);
    }

    match config.provider_id().as_str() {
        "openai_audio_speech" => Ok(Some(Box::new(OpenAiSpeechTtsProvider::new(config.clone())))),
        other => Err(VoiceError::ConfigError(format!(
            "不支持的 TTS provider：'{other}'，当前仅支持 OpenAI-compatible Speech API。"
        ))),
    }
}

/// 根据配置创建 OpenAI-compatible 语音识别器。
pub fn create_speech_recognition_provider(
    config: &SpeechRecognitionConfig,
) -> Result<Option<Box<dyn SpeechRecognitionProvider>>, VoiceError> {
    if !config.enabled() {
        return Ok(None);
    }

    match config.provider_id().as_str() {
        "openai_audio_transcriptions" => Ok(Some(Box::new(OpenAiAudioTranscriptionProvider::new(
            config.clone(),
        )))),
        other => Err(VoiceError::ConfigError(format!(
            "不支持的 ASR provider：'{other}'，当前仅支持 OpenAI-compatible Transcriptions API。"
        ))),
    }
}
