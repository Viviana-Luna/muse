import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { Persona, PersonaMutationResponse } from '@/types';

const client = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  readJson: vi.fn()
}));

vi.mock('./client', () => client);

import { savePersona } from './personas';

const persona: Persona = {
  id: 'alice',
  name: '爱丽丝',
  summary: '',
  character_profile: '可靠',
  world_profile: '现实',
  scenario: '',
  system_prompt: '保持角色。',
  style: '',
  roleplay_style: 'light_narration',
  dialogue_examples: '',
  author_note: '',
  opening_message: '',
  tool_policy: { mode: 'inherit', allowed_tools: [] },
  skill_policy: { mode: 'inherit', allowed_skills: [] },
  mcp_policy: { mode: 'inherit', allowed_servers: [] },
  default_visual_pack_id: 'visual-alice',
  author: '',
  version: '1.0.0',
  notes: ''
};

const mutationResponse: PersonaMutationResponse = {
  affected_persona: persona,
  active_persona: persona,
  active_persona_id: persona.id,
  visual_pack: null,
  runtime_reset: true,
  conversation_id: 'default',
  active_conversation_id: 'default',
  session_restored: false,
  state_revision: 2
};

describe('savePersona', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    client.apiFetch.mockResolvedValue(new Response('{}'));
    client.readJson.mockResolvedValue(mutationResponse);
  });

  it('首个角色把创建并启用意图放进同一次后端请求', async () => {
    await savePersona(persona, false, undefined, { activateAfterCreate: true });

    expect(client.apiFetch).toHaveBeenCalledOnce();
    const [url, init] = client.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('/api/personas');
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toEqual({
      persona,
      activate_after_create: true
    });
  });

  it('编辑角色不会携带创建后启用语义', async () => {
    await savePersona(persona, true, undefined, { activateAfterCreate: true });

    const [, init] = client.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({
      persona,
      activate_after_create: false
    });
  });
});
