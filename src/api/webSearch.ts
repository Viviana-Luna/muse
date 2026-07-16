import type { WebSearchConfig, WebSearchConfigUpdate } from '@/types';

import { apiFetch, readJson } from './client';

/// 读取联网搜索服务的凭据配置状态，响应不包含 API 密钥。
export async function fetchWebSearchConfig(): Promise<WebSearchConfig> {
  return readJson<WebSearchConfig>(await apiFetch('/api/web-search/config'));
}

/// 更新 Brave Search 密钥；后端只操作系统凭据库，不会回传明文。
export async function saveWebSearchConfig(payload: WebSearchConfigUpdate): Promise<WebSearchConfig> {
  return readJson<WebSearchConfig>(
    await apiFetch('/api/web-search/config', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    })
  );
}
