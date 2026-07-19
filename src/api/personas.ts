import type {
  ActivePersonaResponse,
  ActivePersonaStateResponse,
  AssetUploadResponse,
  Persona,
  PersonaCard,
  PersonaCardImportResponse,
  PersonaDeletionImpactResponse,
  PersonaListResponse,
  PersonaMutationResponse,
  PersonaVisualPackPatch
} from '@/types';

import { apiFetch, readJson } from './client';

// 角色系统 API，负责角色列表、激活角色、角色编辑和角色卡片导入导出。
export async function fetchPersonas(): Promise<PersonaListResponse> {
  return readJson<PersonaListResponse>(await apiFetch('/api/personas'));
}

export async function fetchActivePersona(): Promise<ActivePersonaStateResponse> {
  return readJson<ActivePersonaStateResponse>(await apiFetch('/api/personas/active'));
}

export async function fetchPersona(id: string): Promise<ActivePersonaResponse> {
  return readJson<ActivePersonaResponse>(await apiFetch(`/api/personas/${encodeURIComponent(id)}`));
}

export async function savePersona(
  persona: Persona,
  editing: boolean,
  visualPackPatch?: PersonaVisualPackPatch,
  options: { activateAfterCreate?: boolean } = {}
): Promise<PersonaMutationResponse> {
  const url = editing ? `/api/personas/${encodeURIComponent(persona.id)}` : '/api/personas';
  return readJson<PersonaMutationResponse>(
    await apiFetch(url, {
      method: editing ? 'PUT' : 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        persona,
        visual_pack_patch: visualPackPatch,
        activate_after_create: !editing && options.activateAfterCreate === true
      })
    })
  );
}

export async function uploadPersonaImage(file: File): Promise<AssetUploadResponse> {
  const form = new FormData();
  form.append('file', file);
  return readJson<AssetUploadResponse>(
    await apiFetch('/api/assets/upload', {
      method: 'POST',
      body: form
    })
  );
}

export async function discardUnreferencedPersonaImage(url: string): Promise<void> {
  const prefix = '/api/assets/uploaded/';
  if (!url.startsWith(prefix)) {
    throw new Error('只能回滚由 Muse 上传的角色图片。');
  }
  const filename = url.slice(prefix.length);
  if (!filename || filename.includes('/')) {
    throw new Error('待回滚的角色图片地址无效。');
  }
  const response = await apiFetch(
    `/api/assets/uploaded/${encodeURIComponent(filename)}`,
    { method: 'DELETE' }
  );
  if (!response.ok) await readJson(response);
}

export async function deletePersona(id: string): Promise<PersonaMutationResponse> {
  return readJson<PersonaMutationResponse>(
    await apiFetch(`/api/personas/${encodeURIComponent(id)}`, { method: 'DELETE' })
  );
}

export async function fetchPersonaDeletionImpact(
  id: string
): Promise<PersonaDeletionImpactResponse> {
  return readJson<PersonaDeletionImpactResponse>(
    await apiFetch(`/api/personas/${encodeURIComponent(id)}/deletion-impact`)
  );
}

export async function activatePersona(id: string): Promise<PersonaMutationResponse> {
  return readJson<PersonaMutationResponse>(
    await apiFetch(`/api/personas/${encodeURIComponent(id)}/activate`, { method: 'POST' })
  );
}

export async function exportPersonaCard(
  id: string,
  level: PersonaCard['export_level'] = 'with_visual_pack_ref'
): Promise<PersonaCard> {
  return readJson<PersonaCard>(
    await apiFetch(`/api/personas/${encodeURIComponent(id)}/card?level=${encodeURIComponent(level)}`)
  );
}

export interface PersonaCardImportOptions {
  conflictStrategy: 'cancel' | 'overwrite' | 'rename';
  activateAfterImport: boolean;
}

export async function importPersonaCard(
  card: PersonaCard,
  options: PersonaCardImportOptions = { conflictStrategy: 'rename', activateAfterImport: true }
): Promise<PersonaCardImportResponse> {
  return readJson<PersonaCardImportResponse>(
    await apiFetch('/api/personas/import', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        card,
        conflict_strategy: options.conflictStrategy,
        activate_after_import: options.activateAfterImport
      })
    })
  );
}
