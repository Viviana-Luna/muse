// 联网搜索配置只暴露提供器和凭据是否存在，真实密钥始终留在系统凭据库。

export interface WebSearchConfig {
  provider: string;
  api_key_configured: boolean;
}

export type { SecretUpdate as WebSearchConfigUpdate } from './secrets';
