//! 语音能力、合成与识别 HTTP 适配。

use super::*;

/// 语音能力探测：前端据此决定语音按钮是否可用。
pub(crate) async fn handle_voice_capabilities(
    State(state): State<Arc<AppState>>,
) -> Json<VoiceCapabilitiesResponse> {
    let tts_config = {
        let config = state.model_config.lock().await;
        config.tts().clone()
    };
    let tts_available =
        tts_config.is_external_provider() && effective_tts_config(&tts_config).enabled();
    Json(VoiceCapabilitiesResponse {
        tts: tts_available,
        speech_recognition: state.speech_recognition_provider.lock().await.is_some(),
    })
}

/// 调用已配置的 OpenAI-compatible 语音合成服务。
pub(crate) async fn handle_tts_runtime_route(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TtsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    require_active_persona_http(&state).await?;
    let text = req.text.trim();
    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "合成文本不能为空。".to_string(),
            }),
        ));
    }

    tracing::info!(
        target: "agent_vp::voice",
        action = "tts_runtime_route",
        voice_id = req.voice_id.as_deref().unwrap_or("active"),
        text_chars = text.chars().count(),
        "收到语音合成请求"
    );
    let context = resolve_tts_request_context(&state, req.voice_id.as_deref()).await?;
    let provider = if context.effective_tts.is_external_provider() {
        muse_core::speech::factory::create_tts_provider(&context.effective_tts)
            .map_err(voice_error_response)?
            .ok_or_else(|| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse {
                        error: "外接 TTS 服务未启用，请先在设置页配置语音服务。".to_string(),
                    }),
                )
            })?
    } else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "请先配置 OpenAI-compatible 语音合成服务。".to_string(),
            }),
        ));
    };
    let audio = provider
        .synthesize(text)
        .await
        .map_err(voice_error_response)?;

    tracing::info!(
        target: "agent_vp::voice",
        action = "tts_runtime_route",
        content_type = %audio.content_type,
        audio_bytes = audio.bytes.len(),
        "语音合成完成"
    );
    let mut response = Response::new(Body::from(audio.bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&audio.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Ok(response)
}

/// 语音转文本：接收 multipart 音频文件并调用远程转写服务。
///
/// 识别服务未启用时返回 503。表单字段 `audio` 为音频二进制，
/// 可选 `format` 字段为 MIME（默认 audio/webm）。
pub(crate) async fn handle_speech_transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<SpeechTranscriptionResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_active_persona_http(&state).await?;
    let provider = match state.speech_recognition_provider.lock().await.clone() {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "语音识别服务未启用，请先配置 OpenAI-compatible 转写服务。".to_string(),
                }),
            ));
        }
    };

    let (audio, audio_format, _) = parse_transcribe_audio_multipart(&mut multipart).await?;
    tracing::info!(
        target: "agent_vp::voice",
        action = "speech_transcribe",
        audio_format = %audio_format,
        audio_bytes = audio.len(),
        "收到语音转文本请求"
    );

    let text = provider
        .transcribe(audio, &audio_format)
        .await
        .map_err(voice_error_response)?;
    tracing::info!(
        target: "agent_vp::voice",
        action = "speech_transcribe",
        text_chars = text.chars().count(),
        "语音转文本完成"
    );

    Ok(Json(SpeechTranscriptionResponse { text }))
}

async fn parse_transcribe_audio_multipart(
    multipart: &mut axum::extract::Multipart,
) -> Result<(Vec<u8>, String, String), (StatusCode, Json<ErrorResponse>)> {
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut declared_content_type = None::<String>;
    let mut requested_format = None::<String>;
    let mut filename = "recording.wav".to_string();

    loop {
        let Some(field) = multipart
            .next_field()
            .await
            .map_err(|err| bad_request(&format!("读取语音上传表单失败：{err}")))?
        else {
            break;
        };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "audio" => {
                if let Some(ct) = field.content_type() {
                    declared_content_type = Some(ct.to_string());
                }
                if let Some(upload_name) = field.file_name() {
                    let trimmed = upload_name.trim();
                    if !trimmed.is_empty() {
                        filename = trimmed.to_string();
                    }
                }
                audio_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|err| {
                            (
                                StatusCode::BAD_REQUEST,
                                Json(ErrorResponse {
                                    error: format!("读取音频字段失败：{err}"),
                                }),
                            )
                        })?
                        .to_vec(),
                );
            }
            "format" => {
                let value = field
                    .text()
                    .await
                    .map_err(|err| {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(ErrorResponse {
                                error: format!("读取 format 字段失败：{err}"),
                            }),
                        )
                    })?
                    .trim()
                    .to_string();
                if !value.is_empty() {
                    requested_format = Some(value);
                }
            }
            _ => {}
        }
    }

    let audio = audio_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "缺少 audio 字段。".to_string(),
            }),
        )
    })?;
    let audio_format = validate_transcribe_audio(
        &audio,
        declared_content_type.as_deref(),
        requested_format.as_deref(),
    )
    .map_err(|message| bad_request(&message))?;
    Ok((audio, audio_format.to_string(), filename))
}

pub(super) fn validate_transcribe_audio(
    audio: &[u8],
    declared_content_type: Option<&str>,
    requested_format: Option<&str>,
) -> Result<&'static str, String> {
    if audio.len() > MAX_SPEECH_UPLOAD_BYTES {
        return Err(format!(
            "音频文件超过 {} 字节上限。",
            MAX_SPEECH_UPLOAD_BYTES
        ));
    }
    if !is_pcm_wav_container(audio) {
        return Err("音频文件不是有效的 PCM WAV 容器。".to_string());
    }
    for (label, declared) in [
        ("Content-Type", declared_content_type),
        ("format", requested_format),
    ] {
        if let Some(value) = declared {
            let mime = value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !matches!(mime.as_str(), "audio/wav" | "audio/wave" | "audio/x-wav") {
                return Err(format!(
                    "音频真实类型为 audio/wav，但 {label} 声明为 `{value}`。"
                ));
            }
        }
    }
    Ok("audio/wav")
}

fn is_pcm_wav_container(audio: &[u8]) -> bool {
    if audio.len() < 44 || &audio[..4] != b"RIFF" || &audio[8..12] != b"WAVE" {
        return false;
    }
    let declared_size = u32::from_le_bytes([audio[4], audio[5], audio[6], audio[7]]) as usize;
    let Some(container_end) = declared_size.checked_add(8) else {
        return false;
    };
    if container_end < 12 || container_end > audio.len() {
        return false;
    }

    let mut offset = 12usize;
    let mut pcm_format = false;
    let mut nonempty_data = false;
    while offset.saturating_add(8) <= container_end {
        let chunk_id = &audio[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            audio[offset + 4],
            audio[offset + 5],
            audio[offset + 6],
            audio[offset + 7],
        ]) as usize;
        let data_start = offset + 8;
        let Some(data_end) = data_start.checked_add(chunk_size) else {
            return false;
        };
        if data_end > container_end {
            return false;
        }
        if chunk_id == b"fmt " && chunk_size >= 16 {
            let format = u16::from_le_bytes([audio[data_start], audio[data_start + 1]]);
            pcm_format = format == 1;
        } else if chunk_id == b"data" {
            nonempty_data = chunk_size > 0;
        }
        let Some(next) = data_end.checked_add(chunk_size % 2) else {
            return false;
        };
        offset = next;
    }
    pcm_format && nonempty_data
}
