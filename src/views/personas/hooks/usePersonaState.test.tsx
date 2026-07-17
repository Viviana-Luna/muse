import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { createPersonaVisualDraft, usePersonaState } from './usePersonaState';
import type {
  Persona,
  PersonaLibraryItem,
  PersonaVisualPackPatch,
  VisualPack
} from '@/types';

const initialPersona: Persona = {
  id: 'starter',
  name: '初始角色',
  summary: '',
  character_profile: '稳定',
  world_profile: '现实',
  scenario: '',
  system_prompt: '保持角色。',
  style: '',
  roleplay_style: 'light_narration',
  dialogue_examples: '',
  author_note: '',
  opening_message: '你好',
  tool_policy: { mode: 'inherit', allowed_tools: [] },
  skill_policy: { mode: 'inherit', allowed_skills: [] },
  mcp_policy: { mode: 'inherit', allowed_servers: [] },
  preferred_model_ref: null,
  preferred_voice_id: null,
  default_visual_pack_id: 'default',
  author: '',
  version: '1.0.0',
  notes: ''
};

const initialVisualPackDraft: PersonaVisualPackPatch = {
  portrait_path: '',
  theme_color: '#d8596f'
};

const summaries: PersonaLibraryItem[] = [
  {
    id: 'alice',
    name: '爱丽丝',
    summary: '活跃角色',
    default_visual_pack_id: 'alice-visual',
    author: 'Muse',
    version: '1.0.0',
    visual_preview: {
      avatar_path: '/assets/alice-avatar.png',
      portrait_path: '/assets/alice-portrait.png'
    }
  },
  {
    id: 'bob',
    name: '鲍勃',
    summary: '备用角色',
    default_visual_pack_id: 'bob-visual',
    author: 'Muse',
    version: '1.0.0',
    visual_preview: {
      avatar_path: null,
      portrait_path: '/assets/bob-portrait.png'
    }
  }
];

describe('usePersonaState', () => {
  it('编辑草稿不会用立绘静默填充头像或背景', () => {
    const visualPack: VisualPack = {
      id: 'visual-starter',
      name: '初始展示包',
      portrait_path: '/assets/portrait.webp',
      background_path: '',
      avatar_path: '',
      theme_color: '#d8596f',
      theme_mode: 'dark',
      layout_mode: 'portrait-right',
      portrait_frame: 'portrait',
      portrait_fit: 'cover',
      portrait_position_x: 50,
      portrait_position_y: 50,
      portrait_scale: 100,
      fallback_text: '暂无图片',
      version: '1.0.0',
      notes: ''
    };

    expect(createPersonaVisualDraft(visualPack)).toMatchObject({
      portrait_path: '/assets/portrait.webp',
      background_path: '',
      avatar_path: ''
    });
  });

  it('原子应用角色列表、活动角色与筛选条件', () => {
    const { result } = renderHook(() =>
      usePersonaState({ initialPersona, initialVisualPackDraft })
    );

    act(() => result.current.applyPersonaList(summaries, 'alice'));
    expect(result.current.personaList).toEqual(summaries);
    expect(result.current.activePersonaId).toBe('alice');

    act(() => result.current.setStatusFilter('inactive'));
    expect(result.current.filteredPersonas.map((persona) => persona.id)).toEqual(['bob']);

    act(() =>
      result.current.applyActivePersona({
        persona: { ...initialPersona, id: 'bob', name: '鲍勃' },
        visual_pack: null
      })
    );
    expect(result.current.activePersona?.id).toBe('bob');
    expect(result.current.activePersonaId).toBe('bob');

    act(() => result.current.clearActivePersona());
    expect(result.current.activePersona).toBeNull();
    expect(result.current.activeVisualPack).toBeNull();
  });

  it('成组更新编辑草稿且不会丢失相邻字段', () => {
    const { result } = renderHook(() =>
      usePersonaState({ initialPersona, initialVisualPackDraft })
    );

    act(() => {
      result.current.updateEditor('name', '新名称');
      result.current.updateEditorToolPolicyMode('allow_list');
      result.current.updateEditorAllowedTools('file_search, web_fetch, file_search');
      result.current.updateEditorVisualDraft('portrait_path', '/assets/avatar.png');
    });

    expect(result.current.editor.persona.name).toBe('新名称');
    expect(result.current.editor.persona.tool_policy).toEqual({
      mode: 'allow_list',
      allowed_tools: ['file_search', 'web_fetch', 'file_search']
    });
    expect(result.current.editor.visualPackDraft.portrait_path).toBe('/assets/avatar.png');
    expect(result.current.editor.visualDirty).toBe(true);
  });
});
