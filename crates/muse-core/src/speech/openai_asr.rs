//! OpenAI-compatible 语音识别适配模块。

use async_trait::async_trait;
use reqwest::{header, multipart};

use super::{SpeechRecognitionProvider, SpeechRecognitionResult, VoiceError};
use crate::model::config::SpeechRecognitionConfig;

/// 面向 `POST /v1/audio/transcriptions` 的语音识别提供器。
pub struct OpenAiAudioTranscriptionProvider {
    config: SpeechRecognitionConfig,
    client: reqwest::Client,
}

impl OpenAiAudioTranscriptionProvider {
    pub fn new(config: SpeechRecognitionConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .expect("创建 OpenAI-compatible ASR HTTP 客户端失败");
        Self { config, client }
    }

    fn endpoint(&self) -> Result<String, VoiceError> {
        let api_base = self.config.api_base.trim().trim_end_matches('/');
        if api_base.is_empty() {
            return Err(VoiceError::ConfigError(
                "OpenAI-compatible ASR Base URL 未配置。".to_string(),
            ));
        }
        Ok(format!("{api_base}/audio/transcriptions"))
    }
}

#[async_trait]
impl SpeechRecognitionProvider for OpenAiAudioTranscriptionProvider {
    async fn transcribe(&self, audio: Vec<u8>, audio_format: &str) -> SpeechRecognitionResult {
        if audio.is_empty() {
            return Err(VoiceError::DecodeError("录音内容为空。".to_string()));
        }
        let model = self.config.model.trim();
        if model.is_empty() {
            return Err(VoiceError::ConfigError("ASR 模型名称未配置。".to_string()));
        }

        let content_type = audio_format
            .split(';')
            .next()
            .unwrap_or("audio/webm")
            .trim();
        let file = multipart::Part::bytes(audio)
            .file_name("recording.webm")
            .mime_str(content_type)
            .map_err(|error| VoiceError::ConfigError(format!("录音媒体类型无效：{error}")))?;
        let mut form = multipart::Form::new()
            .part("file", file)
            .text("model", model.to_string())
            .text("response_format", "json".to_string());
        if !self.config.language.trim().is_empty() {
            form = form.text("language", self.config.language.trim().to_string());
        }

        let mut request = self.client.post(self.endpoint()?).multipart(form);
        if let Some(api_key) = self
            .config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            request = request.header(header::AUTHORIZATION, format!("Bearer {api_key}"));
        }
        let response = request.send().await.map_err(|error| {
            VoiceError::NetworkError(if error.is_timeout() {
                "语音识别请求超时，请稍后重试。".to_string()
            } else {
                "无法连接语音识别服务，请检查网络与 API 地址。".to_string()
            })
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(VoiceError::ApiError(format!("上游返回 HTTP {status}。")));
        }
        let payload = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| VoiceError::DecodeError(format!("解析 ASR 响应失败：{error}")))?;
        payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .ok_or_else(|| VoiceError::NoSpeech("ASR 服务未返回可用文本。".to_string()))
    }

    fn name(&self) -> &'static str {
        "openai_audio_transcriptions"
    }
}
