import type { SecretUpdate } from './secrets';

export interface McpSecretFieldUpdate {
  target: string;
  environment_name: string;
  secret: SecretUpdate;
}

export interface McpSecretFieldState {
  target: string;
  environment_name: string;
  configured: boolean;
}

export interface McpStdioTransport {
  type: 'stdio';
  command: string;
  args: string[];
  cwd?: string | null;
  env: Record<string, string>;
  secrets: McpSecretFieldState[];
}

export interface McpHttpTransport {
  type: 'streamable_http';
  url: string;
  headers: Record<string, string>;
  header_secrets: McpSecretFieldState[];
  bearer_token?: McpSecretFieldState | null;
}

export type McpTransportResponse = McpStdioTransport | McpHttpTransport;

export interface McpServerSummary {
  name: string;
  enabled: boolean;
  transport: 'stdio' | 'streamable_http';
  revision: string;
  status: 'not_tested' | 'connected' | 'failed' | string;
}

export interface McpServerDetail {
  name: string;
  enabled: boolean;
  request_timeout_ms?: number | null;
  enabled_tools?: string[] | null;
  disabled_tools: string[];
  transport: McpTransportResponse;
  revision: string;
}

export interface McpStdioTransportUpdate {
  type: 'stdio';
  command: string;
  args: string[];
  cwd?: string | null;
  env: Record<string, string>;
  secrets: McpSecretFieldUpdate[];
}

export interface McpHttpTransportUpdate {
  type: 'streamable_http';
  url: string;
  headers: Record<string, string>;
  header_secrets: McpSecretFieldUpdate[];
  bearer_token?: McpSecretFieldUpdate | null;
}

export interface McpServerDraft {
  name: string;
  enabled: boolean;
  request_timeout_ms?: number | null;
  enabled_tools?: string[] | null;
  disabled_tools: string[];
  transport: McpStdioTransportUpdate | McpHttpTransportUpdate;
}

export interface McpCatalogResponse {
  server: string;
  status: string;
  tools: Array<Record<string, unknown>>;
  resources: Array<Record<string, unknown>>;
  errors: Array<Record<string, unknown>>;
  refreshed_at: string;
}
