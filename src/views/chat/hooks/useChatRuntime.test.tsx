import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Message, Persona, RuntimeEvent, RuntimeStateResponse } from '@/types';

const api = vi.hoisted(() => ({
  answerRuntimeUserQuestion: vi.fn(),
  approveRuntimeTool: vi.fn(),
  cancelRuntimeTool: vi.fn(),
  cancelRuntimeTurn: vi.fn(),
  cancelRuntimeUserQuestion: vi.fn(),
  deleteRuntimeSession: vi.fn(),
  fetchActivePersona: vi.fn(),
  fetchHistory: vi.fn(),
  fetchModelInfo: vi.fn(),
  fetchPersonas: vi.fn(),
  fetchRuntimeContextSnapshot: vi.fn(),
  fetchRuntimeMode: vi.fn(),
  fetchRuntimeSessions: vi.fn(),
  fetchRuntimeState: vi.fn(),
  fetchRuntimeTodos: vi.fn(),
  fetchRuntimeTokenUsage: vi.fn(),
  fetchVoiceCapabilities: vi.fn(),
  forkRuntimeSession: vi.fn(),
  listRuntimeSkills: vi.fn(),
  resetConversation: vi.fn(),
  resumeRuntimeSession: vi.fn(),
  streamRuntimeChat: vi.fn(),
  synthesizeSpeech: vi.fn(),
  transcribeAudio: vi.fn(),
  updateRuntimeMode: vi.fn()
}));

vi.mock('@/api', () => api);

import { shouldApplyVoiceTranscript, useChatRuntime } from './useChatRuntime';

const persona: Persona = {
  id: 'muse',
  name: 'Muse',
  summary: '',
  character_profile: '可靠',
  world_profile: '现实日常',
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
  default_visual_pack_id: 'default',
  author: '',
  version: '1.0.0',
  notes: ''
};

const sessions = ['active', 'history-a', 'history-b'].map((conversationId) => ({
  conversation_id: conversationId,
  exists: true,
  can_resume: true,
  records: 2
}));

function historyMessage(content: string): Message[] {
  return [{ role: 'assistant', content }];
}

describe('useChatRuntime', () => {
  let activeConversationId: string;
  let stateRevision: number;

  beforeEach(() => {
    activeConversationId = 'active';
    stateRevision = 1;
    window.history.replaceState({}, '', '/');
    vi.clearAllMocks();

    api.fetchRuntimeState.mockImplementation((signal?: AbortSignal) => {
      if (signal?.aborted) return Promise.reject(new DOMException('aborted', 'AbortError'));
      const state: RuntimeStateResponse = {
        state_revision: stateRevision,
        active_persona_id: persona.id,
        active_conversation_id: activeConversationId,
        mode: 'daily',
        focus_phase: 'plan',
        busy_turn: null,
        exclusive_operation: null,
        usage_summary: {},
        context_summary: {}
      };
      return Promise.resolve(state);
    });
    api.fetchRuntimeSessions.mockImplementation(() =>
      Promise.resolve({
        sessions,
        active_conversation_id: activeConversationId,
        status: 'ok'
      })
    );
    api.fetchHistory.mockImplementation((conversationId = 'active') =>
      Promise.resolve(historyMessage(`${conversationId}-消息`))
    );
    api.fetchModelInfo.mockResolvedValue({ provider: 'mock', model: 'model' });
    api.fetchPersonas.mockResolvedValue({
      personas: [{ id: persona.id, name: persona.name }],
      active_persona_id: persona.id
    });
    api.fetchActivePersona.mockImplementation(async () => ({
      active_persona: persona,
      active_persona_id: persona.id,
      visual_pack: null,
      state_revision: stateRevision
    }));
    api.fetchRuntimeMode.mockResolvedValue({
      mode: 'daily',
      focus_phase: 'plan',
      tool_preset: 'daily',
      status: 'ok'
    });
    api.fetchRuntimeTodos.mockResolvedValue({ todos: [], status: 'ok' });
    api.fetchRuntimeTokenUsage.mockResolvedValue({
      conversation_id: activeConversationId,
      range: 'day',
      totals: {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        reasoning_tokens: 0,
        server_tool_tokens: 0,
        total_tokens: 0
      },
      records: []
    });
    api.fetchRuntimeContextSnapshot.mockResolvedValue({ snapshot: null });
    api.fetchVoiceCapabilities.mockResolvedValue({
      tts: false,
      speech_recognition: false
    });
    api.listRuntimeSkills.mockResolvedValue({
      skills: [
        {
          name: 'pdf',
          description: '读取和生成 PDF',
          revision: 'pdf-revision',
          source: 'user_store'
        }
      ],
      omitted_skill_count: 0
    });
    api.updateRuntimeMode.mockResolvedValue({
      mode: 'daily',
      focus_phase: 'plan',
      tool_preset: 'daily',
      status: 'ok'
    });
  });

  it('录音开始后任一输入、运行时或角色事实变化都拒绝过期转写', () => {
    const current = { inputRevision: 8, stateRevision: 5, personaId: 'muse' };
    expect(
      shouldApplyVoiceTranscript(current, { ...current, inputRevision: 7 })
    ).toBe(false);
    expect(
      shouldApplyVoiceTranscript(current, { ...current, stateRevision: 4 })
    ).toBe(false);
    expect(
      shouldApplyVoiceTranscript(current, { ...current, personaId: 'other' })
    ).toBe(false);
    expect(shouldApplyVoiceTranscript(current, current)).toBe(true);
  });

  afterEach(() => cleanup());

  function setup(options: { getMutationBlockReason?: () => string | null } = {}) {
    const notify = vi.fn();
    const onPersonaBootstrap = vi.fn();
    const result = renderHook(() =>
      useChatRuntime({ activePersona: persona, notify, onPersonaBootstrap, ...options })
    ).result;
    return { result, notify, onPersonaBootstrap };
  }

  it('初始化时在稳定 revision 上原子载入角色、会话与历史', async () => {
    const { result, onPersonaBootstrap } = setup();

    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    expect(onPersonaBootstrap).toHaveBeenCalledWith(
      expect.objectContaining({
        activePersonaId: persona.id,
        active: expect.objectContaining({ persona })
      })
    );
    expect(result.current.modelLabel).toBe('mock / model');
    expect(result.current.activeConversationId).toBe('active');
    expect(result.current.selectedConversationId).toBe('active');
    expect(result.current.messages[0].content).toBe('active-消息');
  });

  it('重启恢复时优先显示供应商与模型的展示名称', async () => {
    api.fetchModelInfo.mockResolvedValue({
      provider: '火山方舟 Agent Plan',
      model: 'glm-5.2',
      provider_id: 'volcengine_agent_plan',
      provider_name: '火山方舟 Agent Plan',
      model_name: 'GLM 5.2'
    });
    const { result } = setup();

    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    expect(result.current.modelLabel).toBe('火山方舟 Agent Plan / GLM 5.2');
  });

  it('通过后端事实切换日常、工作和计划模式', async () => {
    api.updateRuntimeMode.mockResolvedValue({
      mode: 'focus',
      focus_phase: 'build',
      tool_preset: 'focus_build',
      status: '已切换'
    });
    const { result, notify } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    await act(async () => {
      await result.current.handleRuntimeModeChange('focus_build');
    });

    expect(api.updateRuntimeMode).toHaveBeenCalledWith('focus', 'build');
    expect(result.current.runtimeToolPreset).toBe('focus_build');
    expect(result.current.runtimeModeSwitching).toBe(false);
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({ title: '已切换为工作模式', tone: 'success' })
    );
  });

  it('模式切换失败时保留原有后端事实并提示错误', async () => {
    api.updateRuntimeMode.mockRejectedValue(new Error('运行时忙碌'));
    const { result, notify } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    await act(async () => {
      await result.current.handleRuntimeModeChange('focus_plan');
    });

    expect(result.current.runtimeToolPreset).toBe('daily');
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({
        title: '切换运行模式失败',
        description: '运行时忙碌',
        tone: 'error'
      })
    );
  });

  it('后端为新回合分配真实会话 ID 时不以空历史覆盖流式消息', async () => {
    api.streamRuntimeChat.mockImplementation(async (_request, options) => {
      options.onEvent({
        type: 'turn_started',
        turn_id: 'turn-1',
        conversation_id: 'runtime-conversation-1'
      });
      options.onEvent({ type: 'assistant_message', content: '已经收到你的消息。' });
      options.onEvent({ type: 'done' });
    });
    const { result } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));
    const historyCallsBeforeSend = api.fetchHistory.mock.calls.length;

    act(() => result.current.setInputValue('你好'));
    await act(async () => {
      await result.current.handleSend();
    });

    await waitFor(() => expect(result.current.busy).toBe(false));
    expect(result.current.messages.at(-2)).toMatchObject({ role: 'user', content: '你好' });
    expect(result.current.messages.at(-1)).toMatchObject({
      role: 'assistant',
      content: '已经收到你的消息。',
      status: 'completed'
    });
    expect(result.current.activeConversationId).toBe('runtime-conversation-1');
    expect(result.current.selectedConversationId).toBe('runtime-conversation-1');
    expect(api.fetchHistory).toHaveBeenCalledTimes(historyCallsBeforeSend);
  });

  it('仅在后端完成 Turn 冻结后清除 Skill', async () => {
    let emit: ((event: RuntimeEvent) => void) | undefined;
    let finishStream: (() => void) | undefined;
    api.streamRuntimeChat.mockImplementation(
      async (_request, options) =>
        new Promise<void>((resolve) => {
          emit = options.onEvent;
          finishStream = resolve;
        })
    );
    const { result } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    act(() => result.current.skillPicker.openPicker());
    await waitFor(() => expect(result.current.skillPicker.skills).toHaveLength(1));
    act(() => result.current.skillPicker.selectSkill(result.current.skillPicker.skills[0]));
    act(() => result.current.setInputValue('生成报告'));
    await act(async () => {
      await result.current.handleSend();
    });

    expect(result.current.skillPicker.selectedSkill?.name).toBe('pdf');
    expect(api.streamRuntimeChat).toHaveBeenCalledWith(
      expect.objectContaining({ selected_skill: 'pdf' }),
      expect.any(Object)
    );

    act(() => {
      emit?.({ type: 'turn_started', turn_id: 'turn-with-skill', conversation_id: 'active' });
    });
    expect(result.current.skillPicker.selectedSkill).toBeNull();

    act(() => {
      emit?.({ type: 'done' });
      finishStream?.();
    });
    await waitFor(() => expect(result.current.busy).toBe(false));
  });

  it('后端准备失败时保留已选择的 Skill', async () => {
    api.streamRuntimeChat.mockRejectedValue(new Error('Skill revision 已变化'));
    const { result } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    act(() => result.current.skillPicker.openPicker());
    await waitFor(() => expect(result.current.skillPicker.skills).toHaveLength(1));
    act(() => result.current.skillPicker.selectSkill(result.current.skillPicker.skills[0]));
    act(() => result.current.setInputValue('生成报告'));
    await act(async () => {
      await result.current.handleSend();
    });

    await waitFor(() => expect(result.current.busy).toBe(false));
    expect(result.current.skillPicker.selectedSkill?.name).toBe('pdf');
  });

  it('角色与历史之间 revision 变化时整批重试，不提交旧角色快照', async () => {
    const nextPersona = { ...persona, id: 'muse-v2', name: 'Muse V2' };
    let personaAttempt = 0;
    let historyAttempt = 0;

    api.fetchRuntimeState.mockImplementation(() =>
      Promise.resolve({
        state_revision: stateRevision,
        active_persona_id: stateRevision === 1 ? persona.id : nextPersona.id,
        active_conversation_id: stateRevision === 1 ? 'active' : 'history-b',
        mode: 'daily',
        focus_phase: 'plan',
        busy_turn: null,
        exclusive_operation: null,
        usage_summary: {},
        context_summary: {}
      })
    );
    api.fetchPersonas.mockImplementation(async () => {
      personaAttempt += 1;
      const current = personaAttempt === 1 ? persona : nextPersona;
      return {
        personas: [{ id: current.id, name: current.name }],
        active_persona_id: current.id
      };
    });
    api.fetchActivePersona.mockImplementation(async () => {
      const current = personaAttempt === 1 ? persona : nextPersona;
      return {
        active_persona: current,
        active_persona_id: current.id,
        visual_pack: null,
        state_revision: stateRevision
      };
    });
    api.fetchHistory.mockImplementation(async (conversationId = 'active') => {
      historyAttempt += 1;
      if (historyAttempt === 1) {
        stateRevision = 2;
        return historyMessage('旧 revision 历史');
      }
      return historyMessage(`${conversationId}-新 revision 历史`);
    });

    const { result, onPersonaBootstrap } = setup();
    await waitFor(() => expect(result.current.messages[0]?.content).toBe('history-b-新 revision 历史'));

    expect(api.fetchPersonas).toHaveBeenCalledTimes(2);
    expect(api.fetchHistory).toHaveBeenNthCalledWith(1, 'active');
    expect(api.fetchHistory).toHaveBeenNthCalledWith(2, 'history-b');
    expect(onPersonaBootstrap).toHaveBeenCalledTimes(1);
    expect(onPersonaBootstrap).toHaveBeenCalledWith(
      expect.objectContaining({
        activePersonaId: nextPersona.id,
        active: expect.objectContaining({ persona: nextPersona })
      })
    );
  });

  it('角色重置的任一子读取失败时不会泄漏新 session 或覆盖旧消息', async () => {
    const { result, onPersonaBootstrap } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));
    const initialBootstrapCalls = onPersonaBootstrap.mock.calls.length;
    const previousMessages = result.current.messages;
    const previousTokenUsage = result.current.runtimeTokenUsage;
    const previousTodos = result.current.activeTodos;

    stateRevision = 2;
    activeConversationId = 'new-conversation';
    api.fetchRuntimeSessions.mockResolvedValue({
      sessions,
      active_conversation_id: activeConversationId,
      status: 'ok'
    });
    api.fetchPersonas.mockResolvedValue({
      personas: [{ id: persona.id, name: persona.name }],
      active_persona_id: persona.id
    });
    api.fetchRuntimeTodos.mockResolvedValue({
      todos: [{ id: 'new-todo', content: '不应半提交', status: 'pending' }],
      status: 'ok'
    });
    api.fetchRuntimeTokenUsage.mockResolvedValue({
      ...previousTokenUsage,
      conversation_id: activeConversationId,
      totals: { ...previousTokenUsage!.totals, total_tokens: 99 }
    });
    api.fetchHistory.mockImplementation(async (conversationId = 'active') => {
      if (conversationId === activeConversationId) throw new Error('历史读取失败');
      return historyMessage(`${conversationId}-消息`);
    });

    await act(async () => {
      await expect(
        result.current.synchronizeRuntimeAfterPersonaReset({
          stateRevision,
          active: { persona, visual_pack: null }
        })
      ).rejects.toThrow('历史读取失败');
    });

    expect(result.current.activeConversationId).toBe('active');
    expect(result.current.selectedConversationId).toBe('active');
    expect(result.current.messages).toEqual(previousMessages);
    expect(result.current.runtimeTokenUsage).toEqual(previousTokenUsage);
    expect(result.current.activeTodos).toEqual(previousTodos);
    expect(onPersonaBootstrap).toHaveBeenCalledTimes(initialBootstrapCalls);
  });

  it('候选活动角色 revision 过期时即使 ID 相同也重新读取详情', async () => {
    const { result, onPersonaBootstrap } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));
    const updatedPersona = { ...persona, name: 'Muse 新事实' };
    stateRevision = 2;
    api.fetchActivePersona.mockResolvedValue({
      active_persona: updatedPersona,
      active_persona_id: updatedPersona.id,
      visual_pack: null,
      state_revision: stateRevision
    });

    await act(async () => {
      await result.current.synchronizeRuntimeAfterPersonaReset({
        stateRevision: 1,
        active: { persona, visual_pack: null }
      });
    });

    expect(api.fetchActivePersona).toHaveBeenCalled();
    expect(onPersonaBootstrap).toHaveBeenLastCalledWith(
      expect.objectContaining({
        stateRevision: 2,
        active: expect.objectContaining({ persona: updatedPersona })
      })
    );
  });

  it('快速 A→B 切换会取消 A 请求，且 A 的历史不能覆盖 B', async () => {
    const { result } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    api.fetchHistory.mockImplementation((conversationId = 'active', signal?: AbortSignal) => {
      if (conversationId !== 'history-a') {
        return Promise.resolve(historyMessage(`${conversationId}-消息`));
      }
      return new Promise<Message[]>((resolve, reject) => {
        const timer = window.setTimeout(() => resolve(historyMessage('A-过期消息')), 100);
        signal?.addEventListener(
          'abort',
          () => {
            window.clearTimeout(timer);
            reject(new DOMException('aborted', 'AbortError'));
          },
          { once: true }
        );
      });
    });

    let firstRequest!: Promise<void>;
    act(() => {
      firstRequest = result.current.handleSelectRuntimeSession('history-a');
    });
    await waitFor(() =>
      expect(api.fetchHistory).toHaveBeenCalledWith('history-a', expect.any(AbortSignal))
    );

    await act(async () => {
      await result.current.handleSelectRuntimeSession('history-b');
      await firstRequest;
    });

    expect(result.current.selectedConversationId).toBe('history-b');
    expect(result.current.messages[0].content).toBe('history-b-消息');
    expect(result.current.messages.some((message) => message.content.includes('A-过期'))).toBe(false);
  });

  it('恢复历史会话后以服务端事实刷新 active 会话与上下文', async () => {
    const { result, notify } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));
    await act(async () => {
      await result.current.handleSelectRuntimeSession('history-b');
    });

    api.resumeRuntimeSession.mockImplementation(async (conversationId: string) => {
      activeConversationId = conversationId;
      stateRevision += 1;
      return {
        conversation_id: conversationId,
        restored_messages: 2,
        status: '会话已恢复。'
      };
    });

    await act(async () => {
      await result.current.handleResumeSession();
    });

    expect(api.resumeRuntimeSession).toHaveBeenCalledWith('history-b');
    expect(result.current.activeConversationId).toBe('history-b');
    expect(result.current.selectedConversationId).toBe('history-b');
    expect(result.current.messages[0].content).toBe('history-b-消息');
    expect(notify).toHaveBeenCalledWith(expect.objectContaining({ title: '会话已恢复' }));
  });

  it('独立会话页可按显式 ID 恢复，不需要先覆盖聊天预览状态', async () => {
    const { result } = setup();
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));
    expect(result.current.selectedConversationId).toBe('active');

    api.resumeRuntimeSession.mockImplementation(async (conversationId: string) => {
      activeConversationId = conversationId;
      stateRevision += 1;
      return {
        conversation_id: conversationId,
        restored_messages: 2,
        status: '会话已恢复。'
      };
    });

    await act(async () => {
      await result.current.handleResumeSession('history-a');
    });

    expect(api.resumeRuntimeSession).toHaveBeenCalledWith('history-a');
    expect(result.current.activeConversationId).toBe('history-a');
    expect(result.current.selectedConversationId).toBe('history-a');
    expect(result.current.messages[0].content).toBe('history-a-消息');
  });

  it('角色快照刷新或过期时拒绝所有会话 mutation', async () => {
    const { result, notify } = setup({
      getMutationBlockReason: () => '角色状态正在同步，请稍候。'
    });
    await waitFor(() => expect(result.current.runtimeStatus).toBe(''));

    act(() => result.current.setInputValue('不会发送'));
    await act(async () => {
      await result.current.handleSend();
      await result.current.handleReset();
      await result.current.handleResumeSession();
      await result.current.handleForkSession();
      await result.current.handleDeleteRuntimeSession('history-a');
    });

    expect(api.streamRuntimeChat).not.toHaveBeenCalled();
    expect(api.resetConversation).not.toHaveBeenCalled();
    expect(api.resumeRuntimeSession).not.toHaveBeenCalled();
    expect(api.forkRuntimeSession).not.toHaveBeenCalled();
    expect(api.deleteRuntimeSession).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({ description: '角色状态正在同步，请稍候。' })
    );
  });
});
