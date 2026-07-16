//! OpenAI 兼容语音合成适配模块。
//!
//! 该 provider 面向 `POST /v1/audio/speech` 协议。用户自建的本地 TTS 服务
//! 只要兼容这一接口，也可以通过同一路线接入。

use async_trait::async_trait;
use reqwest::header;
use serde_json::json;

use super::{SynthesizedAudio, TtsProvider, TtsResult, VoiceError};
use crate::model::config::TtsConfig;

/// OpenAI-compatible Speech API 语音合成提供器。
pub struct OpenAiSpeechTtsProvider {
    config: TtsConfig,
    client: reqwest::Client,
}

impl OpenAiSpeechTtsProvider {
    /// 创建 OpenAI 兼容语音合成提供器。
    pub fn new(config: TtsConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("创建 OpenAI 兼容 TTS HTTP 客户端失败");
        Self { config, client }
    }

    fn endpoint(&self) -> Result<String, VoiceError> {
        let api_base = self.config.api_base.trim().trim_end_matches('/');
        if api_base.is_empty() {
            return Err(VoiceError::ConfigError(
                "OpenAI 兼容语音服务 Base URL 未配置。".to_string(),
            ));
        }
        Ok(format!("{api_base}/audio/speech"))
    }
}

#[async_trait]
impl TtsProvider for OpenAiSpeechTtsProvider {
    fn name(&self) -> &'static str {
        "openai_audio_speech"
    }

    async fn synthesize(&self, text: &str) -> TtsResult {
        let endpoint = self.endpoint()?;
        let model = self.config.model.trim();
        let voice = self.config.voice_id.trim();
        let response_format = self.config.response_format.trim();
        if model.is_empty() {
            return Err(VoiceError::ConfigError("TTS 模型名称未配置。".to_string()));
        }
        if voice.is_empty() {
            return Err(VoiceError::ConfigError("TTS 音色名称未配置。".to_string()));
        }
        if response_format.is_empty() {
            return Err(VoiceError::ConfigError("TTS 输出格式未配置。".to_string()));
        }

        let body = json!({
            "model": model,
            "input": text,
            "voice": voice,
            "response_format": response_format,
            "speed": self.config.speed,
        });

        let mut request = self
            .client
            .post(endpoint)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(api_key) = self
            .config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            request = request.header(header::AUTHORIZATION, format!("Bearer {api_key}"));
        }

        let response = request.send().await.map_err(|error| {
            VoiceError::NetworkError(if error.is_timeout() {
                "语音服务请求超时，请稍后重试。".to_string()
            } else {
                "无法连接语音服务，请检查网络与 API 地址。".to_string()
            })
        })?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| content_type_from_format(response_format).to_string());

        if !status.is_success() {
            return Err(VoiceError::ApiError(format!("上游返回 HTTP {status}。")));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|err| VoiceError::DecodeError(format!("读取 TTS 音频响应失败：{err}")))?;
        if bytes.is_empty() {
            return Err(VoiceError::DecodeError(
                "TTS 服务返回了空音频。".to_string(),
            ));
        }
        Ok(SynthesizedAudio {
            bytes: bytes.to_vec(),
            content_type,
        })
    }
}

fn content_type_from_format(format: &str) -> &'static str {
    match format.trim().to_ascii_lowercase().as_str() {
        "wav" => "audio/wav",
        "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "pcm" => "audio/pcm",
        _ => "audio/mpeg",
    }
}
