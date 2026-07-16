// 模型 API 模块，封装模型信息、模型目录、远程模型列表和运行时模型配置请求。

import type {
  FetchModelCatalogRequest,
  FetchModelCatalogResponse,
  ActiveChatModelResponse,
  ModelCatalog,
  ModelCatalogItem,
  ModelCatalogMutation,
  ModelInfo,
  ModelsConfig,
  ProviderCredentialResponse,
  ProviderBalanceRequest,
  ProviderBalanceResponse,
  SecretUpdate
} from '@/types';

import { apiFetch, readJson } from './client';

// 模型系统 API，覆盖当前模型、模型目录、供应商拉取和运行时模型配置。
export async function fetchModelInfo(): Promise<ModelInfo> {
  return readJson<ModelInfo>(await apiFetch('/api/models'));
}

// 读取后端维护的模型能力目录。
export async function fetchModelCatalog(): Promise<ModelCatalog> {
  return readJson<ModelCatalog>(await apiFetch('/api/models/catalog'));
}

// 新增模型。供应商必须已经存在于后端目录。
export async function createCatalogModel(
  model: ModelCatalogMutation
): Promise<ModelCatalogItem> {
  return readJson<ModelCatalogItem>(
    await apiFetch('/api/models/catalog/models', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(model)
    })
  );
}

// 编辑模型展示和能力字段，模型 ID 保持不变。
export async function updateCatalogModel(
  model: ModelCatalogMutation
): Promise<ModelCatalogItem> {
  return readJson<ModelCatalogItem>(
    await apiFetch('/api/models/catalog/models', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(model)
    })
  );
}

// 删除模型目录项；当前活动模型会由后端返回冲突。
export async function deleteCatalogModel(providerId: string, model: string): Promise<void> {
  await apiFetch('/api/models/catalog/models', {
    method: 'DELETE',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ provider_id: providerId, model })
  });
}

// 保存或删除供应商级 API Key。
export async function saveProviderCredential(
  providerId: string,
  update: SecretUpdate
): Promise<ProviderCredentialResponse> {
  return readJson<ProviderCredentialResponse>(
    await apiFetch(`/api/models/providers/${encodeURIComponent(providerId)}/credential`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(update)
    })
  );
}

// 从聊天页切换后续对话使用的供应商与模型。
export async function saveActiveChatModel(
  providerId: string,
  model: string
): Promise<ActiveChatModelResponse> {
  return readJson<ActiveChatModelResponse>(
    await apiFetch('/api/models/active', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ provider_id: providerId, model })
    })
  );
}

// 从指定提供器远程拉取当前可用模型列表。
export async function fetchProviderModels(
  request: FetchModelCatalogRequest
): Promise<FetchModelCatalogResponse> {
  return readJson<FetchModelCatalogResponse>(
    await apiFetch('/api/models/catalog/fetch', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(request)
    })
  );
}

// 检测供应商余额。只有声明支持该能力的供应商会返回有效结果。
export async function fetchProviderBalance(
  request: ProviderBalanceRequest
): Promise<ProviderBalanceResponse> {
  return readJson<ProviderBalanceResponse>(
    await apiFetch('/api/models/provider-balance', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(request)
    })
  );
}

/// 读取三段模型配置。响应只返回密钥是否已配置。
export async function fetchModelsConfig(): Promise<ModelsConfig> {
  return readJson<ModelsConfig>(await apiFetch('/api/models/config'));
}

/// 保存三段模型配置并触发热重建。
/// 密钥通过 `api_key_update` 的 keep、replace、delete 动作更新。
export async function saveModelsConfig(config: ModelsConfig): Promise<ModelsConfig> {
  return readJson<ModelsConfig>(
    await apiFetch('/api/models/config', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(config)
    })
  );
}
