import type { SecretUpdate } from './secrets';

// 联网搜索接口只暴露提供器和凭据是否存在，真实密钥仅保存在受保护的 config.toml。

export type WebSearchProvider = 'exa_free_mcp' | 'exa_api';

export interface WebSearchConfig {
  provider: WebSearchProvider;
  api_key_configured: boolean;
}

export interface WebSearchConfigUpdate extends SecretUpdate {
  provider: WebSearchProvider;
}
