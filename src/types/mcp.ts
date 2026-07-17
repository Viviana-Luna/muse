import type { SecretUpdate } from './secrets';

export interface McpSecretFieldUpdate {
  target: string;
  secret: SecretUpdate;
}

export interface McpSecretFieldState {
  target: string;
  configured: boolean;
}

export type McpApprovalPolicy = 'always_ask' | 'trusted_read_only';

export interface McpStructuredError {
  code: string;
  kind: string;
  message: string;
  retryable: boolean;
  alternatives: string[];
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
  tested_revision?: string | null;
  tool_count: number;
  resource_count: number;
  last_checked_at?: string | null;
  last_error?: McpStructuredError | null;
  policy_reason?: string | null;
}

export interface McpServerDetail {
  name: string;
  enabled: boolean;
  request_timeout_ms?: number | null;
  enabled_tools?: string[] | null;
  disabled_tools: string[];
  approval_policy: McpApprovalPolicy;
  tool_approval_overrides: Record<string, McpApprovalPolicy>;
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
  approval_policy: McpApprovalPolicy;
  tool_approval_overrides: Record<string, McpApprovalPolicy>;
  transport: McpStdioTransportUpdate | McpHttpTransportUpdate;
}

export interface McpConnectionDiagnostic {
  transport: 'stdio' | 'streamable_http' | string;
  protocol_version?: string | null;
  server_info?: Record<string, unknown> | null;
  initialized_at?: string | null;
  healthy: boolean;
  stderr_summary?: string | null;
  last_error?: string | null;
  eviction_reason?: string | null;
}

export interface McpCatalogResponse {
  server: string;
  revision: string;
  status: string;
  tools: Array<Record<string, unknown>>;
  resources: Array<Record<string, unknown>>;
  errors: Array<Record<string, unknown>>;
  refreshed_at: string;
  diagnostic: {
    connection?: McpConnectionDiagnostic | null;
    policy_reason?: string | null;
    redirect_policy: 'disabled' | string;
    proxy_policy: 'direct_only' | string;
    sensitive_headers: 'exact_configured_origin_only' | string;
  };
}
