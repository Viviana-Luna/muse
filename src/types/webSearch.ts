import type { SecretUpdate } from './secrets';

// 联网搜索配置只暴露提供器和凭据是否存在，真实密钥始终留在系统凭据库。

export type WebSearchProvider = 'exa_free_mcp' | 'exa_api';

export interface WebSearchConfig {
  provider: WebSearchProvider;
  api_key_configured: boolean;
}

export interface WebSearchConfigUpdate extends SecretUpdate {
  provider: WebSearchProvider;
}
