// 模型目录与模型运行配置类型，描述供应商、模型能力和运行时模型参数。

import type { SpeechRecognitionConfig, TtsConfigSection } from './voice';
import type { SecretUpdate } from './secrets';

// 当前聊天模型概要。
export interface ModelInfo {
  provider: string;
  model: string;
  provider_id?: string;
  provider_name?: string;
  model_name?: string;
}

export function formatModelInfoLabel(model: ModelInfo): string {
  return `${model.provider_name || model.provider} / ${model.model_name || model.model}`;
}

// 模型提供器目录条目。
export interface ModelProviderCatalog {
  id: string;
  name: string;
  default_api_base: string;
  chat_model_list_url: string;
  tts_model_list_url: string;
  enabled: boolean;
  notes: string;
  supports_balance_check: boolean;
  capabilities: string[];
  model_list_auth: 'required' | 'optional' | 'none' | string;
  allow_custom_base: boolean;
  connection_validation: 'model_list' | 'chat_request' | 'chat_probe' | string;
  status: 'supported' | 'legacy_unsupported' | string;
  api_key_configured: boolean;
  credential_diagnostic?: ModelCredentialDiagnostic;
}

// 旧客户端兼容诊断；Provider Profile 运行时不返回密钥或凭据引用。
export interface ModelCredentialDiagnostic {
  code: string;
  field_path: string;
  message: string;
}

// 模型目录创建与编辑表单。供应商和模型 ID 在编辑时不可变。
export interface ModelCatalogMutation {
  provider_id: string;
  model: string;
  name: string;
  notes: string;
  tags: string[];
  functions: string[];
  context_window: number;
  default_max_output_tokens: number;
  supports_usage: boolean;
  supports_cached_tokens: boolean;
  supports_reasoning_tokens: boolean;
  tokenizer_family: string;
}

// 聊天页切换活动模型后的双要素事实。
export interface ActiveChatModelResponse {
  provider_id: string;
  provider_name: string;
  model: string;
  model_name: string;
}

// 供应商凭据状态，永不包含密钥或凭据引用。
export interface ProviderCredentialResponse {
  provider_id: string;
  api_key_configured: boolean;
  status: string;
  credential_diagnostic?: ModelCredentialDiagnostic;
}

// 模型目录条目。
export interface ModelCatalogItem {
  id: string;
  provider_id: string;
  name: string;
  model: string;
  default_api_base: string;
  enabled: boolean;
  notes: string;
  tags: string[];
  functions: string[];
  capabilities: string[];
  context_window: number;
  default_max_output_tokens: number;
  supports_usage: boolean;
  supports_cached_tokens: boolean;
  supports_reasoning_tokens: boolean;
  tokenizer_family: string;
}

// 完整模型目录响应结构。
export interface ModelCatalog {
  providers: ModelProviderCatalog[];
  models: ModelCatalogItem[];
  capabilities: string[];
}

// 拉取提供器模型列表请求结构。
export interface FetchModelCatalogRequest {
  provider: string;
  purpose: string;
  api_base: string;
  model?: string | null;
  api_key?: string | null;
}

// 拉取提供器模型列表响应结构。
export interface FetchModelCatalogResponse {
  provider_id: string;
  models: ModelCatalogItem[];
}

// 供应商余额检测请求结构。
export interface ProviderBalanceRequest {
  provider: string;
  api_base: string;
  api_key?: string | null;
  purpose?: string | null;
}

// 供应商余额明细。
export interface ProviderBalanceInfo {
  currency: string;
  total_balance: string;
  granted_balance: string;
  topped_up_balance: string;
}

// 供应商余额检测响应结构。
export interface ProviderBalanceResponse {
  provider_id: string;
  is_available: boolean;
  balance_infos: ProviderBalanceInfo[];
  status: string;
}

// ---- 模型配置（运行时热更新） ----
// 对接后端 `GET/PUT /api/models/config`。配置分为聊天、语音合成、语音识别和音频理解。
// `GET` 只返回 `api_key_configured`；输入框中的占位值只存在于前端内存。

/// 单段模型配置。用于聊天与音频理解配置。
export interface ModelConfigSection {
  provider: string;
  api_base: string;
  /// 旧配置兼容字段；当前产品固定使用 Chat Completions。
  api_protocol?: string;
  api_key?: string | null;
  api_key_configured: boolean;
  api_key_update?: SecretUpdate;
  model: string;
  max_tokens?: number | null;
  temperature?: number | null;
  voice_id?: string | null;
  speed?: number | null;
}

/// 运行时模型配置聚合。
export interface ModelsConfig {
  chat: ModelConfigSection;
  tts: TtsConfigSection;
  speech_recognition: SpeechRecognitionConfig;
  audio_understanding: ModelConfigSection;
  voice_input: {
    mode: 'speech_text' | string;
  };
}
