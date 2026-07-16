import { apiFetch, readJson } from './client';
import type { SkillDraft, SkillRecord, SkillSummary, SkillUpdate } from '@/types';

export async function listSkills(): Promise<SkillSummary[]> {
  return readJson(await apiFetch('/api/skills'));
}

export async function getSkill(name: string): Promise<SkillRecord> {
  return readJson(await apiFetch(`/api/skills/${encodeURIComponent(name)}`));
}

export async function createSkill(draft: SkillDraft): Promise<SkillRecord> {
  return readJson(
    await apiFetch('/api/skills', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(draft)
    })
  );
}

export async function updateSkill(currentName: string, draft: SkillUpdate): Promise<SkillRecord> {
  return readJson(
    await apiFetch(`/api/skills/${encodeURIComponent(currentName)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(draft)
    })
  );
}

export async function deleteSkill(name: string, revision: string): Promise<void> {
  const response = await apiFetch(
    `/api/skills/${encodeURIComponent(name)}?revision=${encodeURIComponent(revision)}`,
    { method: 'DELETE' }
  );
  if (!response.ok) await readJson(response);
}
