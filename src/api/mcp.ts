import { apiFetch, readJson } from './client';
import type {
  McpCatalogResponse,
  McpServerDetail,
  McpServerDraft,
  McpServerSummary
} from '@/types';

export async function listMcpServers(): Promise<McpServerSummary[]> {
  return readJson(await apiFetch('/api/mcp/servers'));
}

export async function getMcpServer(name: string): Promise<McpServerDetail> {
  return readJson(await apiFetch(`/api/mcp/servers/${encodeURIComponent(name)}`));
}

export async function createMcpServer(draft: McpServerDraft): Promise<McpServerDetail> {
  return readJson(
    await apiFetch('/api/mcp/servers', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(draft)
    })
  );
}

export async function updateMcpServer(
  currentName: string,
  revision: string,
  draft: McpServerDraft
): Promise<McpServerDetail> {
  return readJson(
    await apiFetch(`/api/mcp/servers/${encodeURIComponent(currentName)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ revision, ...draft })
    })
  );
}

export async function deleteMcpServer(name: string, revision: string): Promise<void> {
  const response = await apiFetch(
    `/api/mcp/servers/${encodeURIComponent(name)}?revision=${encodeURIComponent(revision)}`,
    { method: 'DELETE' }
  );
  if (!response.ok) await readJson(response);
}

export async function testMcpServer(name: string): Promise<McpCatalogResponse> {
  return readJson(
    await apiFetch(`/api/mcp/servers/${encodeURIComponent(name)}/test`, { method: 'POST' })
  );
}

export async function testMcpServerDraft(
  server: McpServerDraft,
  options: {
    sourceName?: string;
    sourceRevision?: string;
    includeResources?: boolean;
  } = {}
): Promise<McpCatalogResponse> {
  return readJson(
    await apiFetch('/api/mcp/servers/test-draft', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        source_name: options.sourceName,
        source_revision: options.sourceRevision,
        include_resources: options.includeResources ?? false,
        server
      })
    })
  );
}

export async function refreshMcpServer(name: string): Promise<McpCatalogResponse> {
  return readJson(
    await apiFetch(`/api/mcp/servers/${encodeURIComponent(name)}/refresh`, { method: 'POST' })
  );
}
