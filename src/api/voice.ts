// 语音 API 模块，封装语音合成、语音识别和能力探测请求。

import { apiFetch, readJson } from './client';

// 后端语音能力开关状态。
export interface VoiceCapabilities {
  tts: boolean;
  speech_recognition: boolean;
}

/// 探测后端语音合成与语音识别是否可用。前端据此决定语音按钮是否可点击。
export async function fetchVoiceCapabilities(): Promise<VoiceCapabilities> {
  return readJson<VoiceCapabilities>(await apiFetch('/api/voice/capabilities'));
}

/// 文本转语音：返回后端生成的音频 Blob。
///
/// 后端语音合成未启用时返回 503，`readJson` 会抛错；调用方据此提示用户。
export async function synthesizeSpeech(
  text: string,
  signal?: AbortSignal,
  voiceId?: string
): Promise<Blob> {
  const response = await apiFetch('/api/tts', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text, voice_id: voiceId || null }),
    signal
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    const message = typeof payload?.error === 'string' ? payload.error : '语音合成失败';
    throw new Error(message);
  }
  return response.blob();
}

/// 语音转文本：上传音频 Blob，返回识别文本。
///
/// `audio` 为前端编码的 16kHz 单声道 PCM WAV，由后端转发给 OpenAI 兼容语音识别接口。
/// 语音识别服务未启用时返回 503。
export async function transcribeAudio(
  audio: Blob,
  format: string,
  signal?: AbortSignal
): Promise<string> {
  const form = new FormData();
  form.append('audio', audio, 'recording');
  form.append('format', format);
  const response = await apiFetch('/api/speech/transcribe', {
    method: 'POST',
    body: form,
    signal
  });
  const payload = (await response.json().catch(() => ({}))) as { text?: string; error?: string };
  if (!response.ok) {
    throw new Error(typeof payload?.error === 'string' ? payload.error : '语音识别失败');
  }
  return typeof payload?.text === 'string' ? payload.text : '';
}
