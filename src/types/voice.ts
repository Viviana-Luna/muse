// 语音相关类型，描述 OpenAI-compatible 语音合成和语音识别配置。

import type { SecretUpdate } from './secrets';

// 语音合成配置段。
export interface TtsConfigSection {
  enabled: boolean;
  provider: 'openai_audio_speech' | string;
  api_base: string;
  api_key?: string | null;
  api_key_configured: boolean;
  api_key_update?: SecretUpdate;
  model: string;
  voice_id: string;
  speed: number;
  response_format: string;
}

// 语音识别配置段。
export interface SpeechRecognitionConfig {
  enabled: boolean;
  provider: 'openai_audio_transcriptions' | string;
  api_base: string;
  api_key?: string | null;
  api_key_configured: boolean;
  api_key_update?: SecretUpdate;
  model: string;
  language: string;
  response_format: string;
}
