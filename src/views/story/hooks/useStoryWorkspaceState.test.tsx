import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import type { Persona } from '@/types';
import { useStoryWorkspaceState } from './useStoryWorkspaceState';

const persona: Persona = {
  id: 'alice',
  name: '爱丽丝',
  summary: '',
  character_profile: '稳定',
  world_profile: '现实',
  scenario: '',
  system_prompt: '保持角色。',
  style: '',
  roleplay_style: 'light_narration',
  dialogue_examples: '',
  author_note: '',
  opening_message: '你好。',
  tool_policy: { mode: 'inherit', allowed_tools: [] },
  skill_policy: { mode: 'inherit', allowed_skills: [] },
  mcp_policy: { mode: 'inherit', allowed_servers: [] },
  preferred_model_ref: null,
  preferred_voice_id: null,
  feature_policy: { emotion_persistence_enabled: true },
  default_visual_pack_id: 'alice-visual',
  author: '',
  version: '1.0.0',
  notes: ''
};

const emptySnapshot = {
  stateRevision: 1,
  personas: { personas: [], active_persona_id: null },
  activePersonaId: null,
  active: null
};

const activeSnapshot = {
  stateRevision: 2,
  personas: {
    personas: [
      {
        id: persona.id,
        name: persona.name,
        summary: persona.summary,
        default_visual_pack_id: persona.default_visual_pack_id,
        author: persona.author,
        version: persona.version,
        visual_preview: {
          avatar_path: null,
          portrait_path: null
        }
      }
    ],
    active_persona_id: persona.id
  },
  activePersonaId: persona.id,
  active: { persona, visual_pack: null }
};

describe('useStoryWorkspaceState', () => {
  it('显式区分冷启动、可信快照、刷新和陈旧快照', () => {
    const { result } = renderHook(() => useStoryWorkspaceState());

    expect(result.current.resourceState.status).toBe('initial');
    expect(result.current.presence).toBeNull();

    act(() => result.current.beginLoad());
    expect(result.current.resourceState.status).toBe('loading');

    act(() => result.current.commitSnapshot(emptySnapshot));
    expect(result.current.resourceState.status).toBe('ready');
    expect(result.current.presence).toEqual({ kind: 'empty_library' });

    act(() => result.current.beginLoad());
    expect(result.current.resourceState).toEqual({ status: 'refreshing', data: emptySnapshot });

    act(() => result.current.failLoad(new Error('同步失败')));
    expect(result.current.resourceState).toEqual({
      status: 'stale',
      data: emptySnapshot,
      error: '同步失败'
    });

    act(() => result.current.commitSnapshot(activeSnapshot));
    expect(result.current.presence).toEqual({
      kind: 'active_persona',
      active: activeSnapshot.active
    });
  });

  it('没有可信快照时失败不会被误判为空角色库', () => {
    const { result } = renderHook(() => useStoryWorkspaceState());

    act(() => result.current.failLoad(new Error('后端不可用')));

    expect(result.current.resourceState).toEqual({
      status: 'failed',
      error: '后端不可用'
    });
    expect(result.current.presence).toBeNull();
  });
});
