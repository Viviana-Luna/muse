import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type {
  Persona,
  PersonaLibraryItem,
  RuntimeStateResponse,
  VisualPack
} from '@/types';
import type {
  RevisionBoundActivePersona,
  StoryRuntimeSnapshot
} from '@/views/story/types';

const api = vi.hoisted(() => ({
  activatePersona: vi.fn(),
  deletePersona: vi.fn(),
  exportPersonaCard: vi.fn(),
  fetchActivePersona: vi.fn(),
  fetchPersona: vi.fn(),
  fetchPersonas: vi.fn(),
  importPersonaCard: vi.fn(),
  savePersona: vi.fn()
}));

vi.mock('@/api', () => api);

import { usePersonaController } from './usePersonaController';
import { usePersonaState } from './usePersonaState';

const persona: Persona = {
  id: 'alice',
  name: '爱丽丝',
  summary: '首个角色',
  character_profile: '沉稳',
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
  default_visual_pack_id: 'visual-alice',
  author: 'Muse',
  version: '1.0.0',
  notes: ''
};

const visualPack: VisualPack = {
  id: 'visual-alice',
  name: '爱丽丝展示包',
  portrait_path: '/alice.png',
  background_path: '/alice-bg.png',
  avatar_path: '/alice.png',
  theme_color: '#887766',
  theme_mode: 'dark',
  layout_mode: 'default',
  portrait_frame: 'portrait',
  portrait_fit: 'cover',
  portrait_position_x: 50,
  portrait_position_y: 50,
  portrait_scale: 100,
  fallback_text: '',
  version: '1.0.0',
  notes: ''
};

function summary(value: Persona): PersonaLibraryItem {
  return {
    id: value.id,
    name: value.name,
    summary: value.summary,
    default_visual_pack_id: value.default_visual_pack_id,
    author: value.author,
    version: value.version,
    visual_preview: {
      avatar_path: value.id === persona.id ? visualPack.avatar_path : null,
      portrait_path: value.id === persona.id ? visualPack.portrait_path : null
    }
  };
}

function runtimeSnapshot(candidate: RevisionBoundActivePersona): StoryRuntimeSnapshot {
  const activePersonaId = candidate.active?.persona.id ?? null;
  return {
    stateRevision: candidate.stateRevision,
    personas: {
      personas: candidate.active ? [summary(candidate.active.persona)] : [],
      active_persona_id: activePersonaId
    },
    activePersonaId,
    active: candidate.active,
    sessions: { sessions: [], active_conversation_id: 'default', status: 'ok' },
    activeConversationId: 'default',
    selectedConversationId: 'default',
    history: [],
    todos: [],
    tokenUsage: {} as StoryRuntimeSnapshot['tokenUsage'],
    contextSnapshot: null,
    approvalMode: {
      conversation_id: 'default',
      preset: 'manual',
      approval_policy: 'on_request',
      approvals_reviewer: 'user',
      permission_profile: 'workspace_write',
      revision: 0,
      status: 'ok'
    }
  };
}

describe('usePersonaController', () => {
  let runtimeState: RuntimeStateResponse;

  beforeEach(() => {
    vi.clearAllMocks();
    runtimeState = {
      state_revision: 1,
      active_persona_id: null,
      active_conversation_id: 'default',
      mode: 'focus',
      focus_phase: 'build',
      busy_turn: null,
      exclusive_operation: null,
      usage_summary: {},
      context_summary: {}
    };
  });

  function setup(options: {
    synchronize?: (
      candidate: RevisionBoundActivePersona
    ) => Promise<StoryRuntimeSnapshot>;
    onCommit?: () => void;
    getMutationBlockReason?: () => string | null;
  } = {}) {
    const notify = vi.fn();
    const setBusy = vi.fn();
    const setRuntimeStatus = vi.fn();
    const showActivePersona = vi.fn();
    const onCommit = vi.fn(options.onCommit);
    let applySynchronizedSnapshot: (snapshot: StoryRuntimeSnapshot) => void = () => undefined;
    const synchronizeRuntimeAfterPersonaReset = vi.fn(async (candidate: RevisionBoundActivePersona) => {
      const snapshot = options.synchronize
        ? await options.synchronize(candidate)
        : runtimeSnapshot(candidate);
      applySynchronizedSnapshot(snapshot);
      return snapshot;
    });
    const stopVoice = vi.fn();
    const closeCurrentChatSource = vi.fn();
    const result = renderHook(() => {
      const state = usePersonaState();
      applySynchronizedSnapshot = (snapshot) => {
        state.applyPersonaList(snapshot.personas.personas, snapshot.activePersonaId);
        if (snapshot.active) state.applyActivePersona(snapshot.active);
        else state.clearActivePersona();
        onCommit();
      };
      const controller = usePersonaController({
        state,
        notify,
        setBusy,
        setRuntimeStatus,
        getMutationBlockReason: options.getMutationBlockReason,
        acceptStateRevision: (revision) => revision >= runtimeState.state_revision,
        loadStableRuntimeState: async (load) => ({
          value: await load(runtimeState),
          state: runtimeState
        }),
        showActivePersona,
        synchronizeRuntimeAfterPersonaReset,
        closeCurrentChatSource,
        stopVoice,
        navigateToStory: vi.fn(),
        onPersonaRefreshSuccess: onCommit
      });
      return { state, controller };
    }).result;
    return {
      result,
      notify,
      setBusy,
      setRuntimeStatus,
      showActivePersona,
      onCommit,
      synchronizeRuntimeAfterPersonaReset,
      closeCurrentChatSource,
      stopVoice
    };
  }

  it('首个角色通过单次创建并启用 mutation，同步完整快照后才提交', async () => {
    let resolveSync!: () => void;
    const syncGate = new Promise<void>((resolve) => {
      resolveSync = resolve;
    });
    const order: string[] = [];
    api.savePersona.mockImplementation(async () => {
      order.push('create');
      runtimeState = {
        ...runtimeState,
        state_revision: 2,
        active_persona_id: persona.id
      };
      return {
        affected_persona: persona,
        active_persona: persona,
        active_persona_id: persona.id,
        visual_pack: visualPack,
        runtime_reset: true,
        conversation_id: 'default',
        state_revision: 2
      };
    });
    const setupResult = setup({
      synchronize: async (candidate) => {
        order.push('sync');
        await syncGate;
        return runtimeSnapshot(candidate);
      },
      onCommit: () => order.push('commit')
    });

    let savePromise!: Promise<boolean>;
    act(() => {
      savePromise = setupResult.result.current.controller.saveEditorDraft(persona, 'create');
    });
    await waitFor(() => expect(api.savePersona).toHaveBeenCalled());
    await waitFor(() => expect(order).toContain('sync'));
    expect(setupResult.result.current.state.activePersona).toBeNull();
    expect(setupResult.onCommit).not.toHaveBeenCalled();

    resolveSync();
    await act(async () => expect(await savePromise).toBe(true));

    expect(order).toEqual(['create', 'sync', 'commit']);
    expect(api.activatePersona).not.toHaveBeenCalled();
    expect(api.savePersona).toHaveBeenCalledWith(
      persona,
      false,
      undefined,
      { activateAfterCreate: true }
    );
    expect(setupResult.closeCurrentChatSource).toHaveBeenCalledOnce();
    expect(setupResult.stopVoice).toHaveBeenCalledOnce();
    expect(setupResult.result.current.state.activePersona).toEqual(persona);
    expect(setupResult.result.current.state.activeVisualPack).toEqual(visualPack);
    expect(setupResult.setRuntimeStatus).toHaveBeenCalledWith('角色已创建并启用。');
  });

  it('后端未完成原子启用时保留编辑器并切换为已创建角色的编辑模式', async () => {
    api.savePersona.mockResolvedValue({
      affected_persona: persona,
      active_persona: null,
      active_persona_id: null,
      visual_pack: null,
      runtime_reset: false,
      conversation_id: 'default',
      state_revision: 1
    });
    api.fetchPersonas.mockResolvedValue({
      personas: [summary(persona)],
      active_persona_id: null
    });
    const setupResult = setup();

    await act(async () => {
      await expect(
        setupResult.result.current.controller.saveEditorDraft(persona, 'create')
      ).rejects.toThrow('后端未返回“创建并启用”的完整事实结果');
    });

    expect(api.activatePersona).not.toHaveBeenCalled();
    expect(setupResult.result.current.state.editor.mode).toBe('edit');
    expect(setupResult.result.current.state.editor.persona).toEqual(persona);
    expect(setupResult.result.current.state.personaList).toEqual([summary(persona)]);
    expect(setupResult.result.current.controller.transitionState.status).toBe('failed');
  });

  it('删除非活动角色后使用响应中同源的当前角色与展示包', async () => {
    const other = { ...persona, id: 'bob', name: '鲍勃', default_visual_pack_id: 'visual-bob' };
    runtimeState = { ...runtimeState, active_persona_id: persona.id };
    api.deletePersona.mockResolvedValue({
      affected_persona: other,
      active_persona: persona,
      active_persona_id: persona.id,
      visual_pack: visualPack,
      runtime_reset: false,
      conversation_id: 'default',
      state_revision: 1
    });
    api.fetchPersonas.mockResolvedValue({
      personas: [summary(persona)],
      active_persona_id: persona.id
    });
    const setupResult = setup();
    act(() => {
      setupResult.result.current.state.applyPersonaList(
        [summary(persona), summary(other)],
        persona.id
      );
      setupResult.result.current.state.applyActivePersona({ persona, visual_pack: visualPack });
    });

    await act(async () => setupResult.result.current.controller.handleDelete(other.id));

    expect(api.fetchActivePersona).not.toHaveBeenCalled();
    expect(setupResult.result.current.state.activePersona).toEqual(persona);
    expect(setupResult.result.current.state.activeVisualPack).toEqual(visualPack);
    expect(setupResult.synchronizeRuntimeAfterPersonaReset).not.toHaveBeenCalled();
  });

  it('资源陈旧时从控制器层拒绝角色 mutation', async () => {
    const setupResult = setup({ getMutationBlockReason: () => '角色状态已过期，请先重试同步。' });

    await act(async () => {
      await setupResult.result.current.controller.handleActivate(persona.id);
      await setupResult.result.current.controller.handleDelete(persona.id);
      await setupResult.result.current.controller.saveEditorDraft(persona, 'create');
    });

    expect(api.activatePersona).not.toHaveBeenCalled();
    expect(api.deletePersona).not.toHaveBeenCalled();
    expect(api.savePersona).not.toHaveBeenCalled();
    expect(setupResult.notify).toHaveBeenCalledWith(
      expect.objectContaining({ description: '角色状态已过期，请先重试同步。' })
    );
  });
});
