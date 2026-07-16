import { useCallback, useEffect, useRef, useState } from 'react';

import {
  deleteRuntimeSession,
  fetchActivePersona,
  fetchHistory,
  fetchModelInfo,
  fetchPersonas,
  fetchRuntimeContextSnapshot,
  fetchRuntimeMode,
  fetchRuntimeSessions,
  fetchRuntimeState,
  fetchRuntimeTodos,
  fetchRuntimeTokenUsage,
  forkRuntimeSession,
  resetConversation,
  resumeRuntimeSession
} from '@/api';
import type { AppToastInput } from '@/hooks/useAppToast';
import { formatModelInfoLabel } from '@/types';
import {
  toChatMessages,
  useRuntimeStream,
  type ChatMessage,
  type RuntimeDialogueLine
} from '@/hooks/useRuntimeStream';
import {
  useVoiceRuntime,
  type VoiceTranscriptContext
} from '@/hooks/useVoiceRuntime';
import type {
  ActivePersonaResponse,
  Persona,
  RuntimeStateResponse,
  RuntimeTokenUsage
} from '@/types';
import {
  normalizeConversationId,
  readConversationIdFromUrl,
  resolveSelectedConversationId,
  useSessionState
} from '@/views/hooks/useSessionState';
import {
  loadAtStableRevision,
  useRuntimeStateRevision
} from '@/views/hooks/useRuntimeStateRevision';
import type {
  RevisionBoundActivePersona,
  StoryPersonaSnapshot,
  StoryRuntimeSnapshot
} from '@/views/story/types';
import { useFollowLatest } from './useFollowLatest';
import { useRuntimeOverview } from './useRuntimeOverview';

export type PersonaBootstrapSnapshot = StoryPersonaSnapshot;

interface UseChatRuntimeOptions {
  activePersona: Persona | null;
  notify: (input: AppToastInput) => void;
  getMutationBlockReason?: () => string | null;
  onPersonaBootstrapStart?: () => void;
  onPersonaBootstrap: (snapshot: PersonaBootstrapSnapshot) => void;
  onPersonaBootstrapError?: (error: unknown) => void;
}

const INITIAL_DIALOGUE: RuntimeDialogueLine = {
  speaker: '系统',
  text: '舞台正在准备中。',
  role: 'system'
};

export function shouldApplyVoiceTranscript(
  current: VoiceTranscriptContext,
  captured: VoiceTranscriptContext
) {
  return (
    current.inputRevision === captured.inputRevision &&
    current.stateRevision === captured.stateRevision &&
    current.personaId === captured.personaId
  );
}

function lastDialogueLine(
  messages: ChatMessage[],
  active: Persona | null,
  fallback = '新的对话已经开始。'
): RuntimeDialogueLine {
  const lastLine = [...messages].reverse().find((message) => message.content.trim());
  if (lastLine) {
    return {
      speaker: lastLine.role === 'user' ? '你' : active?.name || '当前角色',
      text: lastLine.content,
      role: lastLine.role === 'user' ? 'user' : 'assistant'
    };
  }
  return {
    speaker: active?.name || '系统',
    text: active?.opening_message || fallback,
    role: active ? 'assistant' : 'system'
  };
}

/**
 * 聊天运行时控制器。
 *
 * 这里统一编排流式回合、语音、会话 mutation、历史读取与 revision 门闩，页面只消费
 * 后端事实状态和事件处理器，避免异步 A 会话响应覆盖已经选中的 B 会话。
 */
export function useChatRuntime(options: UseChatRuntimeOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const [bootstrapAttempt, setBootstrapAttempt] = useState(0);

  const [modelLabel, setModelLabel] = useState('正在读取模型信息');
  const [dialogue, setDialogue] = useState<RuntimeDialogueLine>(INITIAL_DIALOGUE);
  const [inputValue, setInputValueState] = useState('');
  const inputRevisionRef = useRef(0);
  const setInputValue = useCallback((value: string) => {
    inputRevisionRef.current += 1;
    setInputValueState(value);
  }, []);
  const [runtimeMode, setRuntimeMode] = useState('daily');
  const [runtimeFocusPhase, setRuntimeFocusPhase] = useState('plan');
  const [runtimeToolPreset, setRuntimeToolPreset] = useState('daily');

  const session = useSessionState();
  const revision = useRuntimeStateRevision();
  const overview = useRuntimeOverview();
  const currentVoiceTranscriptContext = useCallback(
    (): VoiceTranscriptContext => ({
      inputRevision: inputRevisionRef.current,
      stateRevision: revision.currentStateRevision(),
      personaId: optionsRef.current.activePersona?.id ?? null
    }),
    [revision.currentStateRevision]
  );
  const {
    voice,
    audioRef,
    visualizerCanvasRef,
    speak,
    toggleVoice,
    stopVoice,
    startVoiceInput,
    refreshVoiceCapabilities
  } = useVoiceRuntime({
    getTranscriptContext: currentVoiceTranscriptContext,
    onTranscript: (transcript, capturedContext) => {
      if (!shouldApplyVoiceTranscript(currentVoiceTranscriptContext(), capturedContext)) return;
      setInputValue(transcript);
      setDialogue({ speaker: '你', text: transcript, role: 'user' });
    },
    onNotify: options.notify
  });

  const loadStableRuntimeState = useCallback(
    async <T,>(load: (stableState: RuntimeStateResponse) => Promise<T>) =>
      loadAtStableRevision({
        load,
        readState: () => fetchRuntimeState(),
        currentRevision: revision.currentStateRevision,
        acceptRevision: revision.acceptStateRevision
      }),
    [revision.acceptStateRevision, revision.currentStateRevision]
  );

  function applyRuntimeMode(mode: string, focusPhase: string, toolPreset: string) {
    setRuntimeMode(mode);
    setRuntimeFocusPhase(focusPhase);
    setRuntimeToolPreset(toolPreset);
  }

  function showActivePersona(active: ActivePersonaResponse) {
    const opening = active.persona.opening_message.trim() || '我在这里。';
    setDialogue({
      speaker: active.persona.name || '当前角色',
      text: opening,
      role: 'assistant'
    });
  }

  function resolveActivePersona(
    activeState: Awaited<ReturnType<typeof fetchActivePersona>>,
    activePersonaId: string | null
  ): ActivePersonaResponse | null {
    if (activePersonaId === null) {
      if (activeState.active_persona_id !== null || activeState.active_persona !== null) {
        throw new Error('活动角色接口与运行时空角色事实不一致。');
      }
      return null;
    }
    if (
      activeState.active_persona_id !== activePersonaId ||
      activeState.active_persona?.id !== activePersonaId
    ) {
      throw new Error('活动角色接口与运行时角色事实不一致。');
    }
    return {
      persona: activeState.active_persona,
      visual_pack: activeState.visual_pack
    };
  }

  async function loadStoryRuntimeSnapshot(options: {
    candidateActive?: RevisionBoundActivePersona;
    requestedConversationId?: string | null;
  } = {}): Promise<StoryRuntimeSnapshot> {
    const { value, state } = await loadStableRuntimeState(async (stableState) => {
      const [personas, sessions, todos] = await Promise.all([
        fetchPersonas(),
        fetchRuntimeSessions(),
        fetchRuntimeTodos()
      ]);
      const candidatePersonaId = options.candidateActive?.active?.persona.id ?? null;
      const canReuseCandidate =
        options.candidateActive !== undefined &&
        options.candidateActive.stateRevision === stableState.state_revision &&
        candidatePersonaId === stableState.active_persona_id &&
        personas.active_persona_id === stableState.active_persona_id;
      const activeState = canReuseCandidate ? null : await fetchActivePersona();
      const active = canReuseCandidate ? options.candidateActive!.active : null;
      const activeConversationId = normalizeConversationId(stableState.active_conversation_id);
      const selectedConversationId = resolveSelectedConversationId(
        options.requestedConversationId,
        sessions.sessions,
        activeConversationId
      );
      const [history, tokenUsage, context] = await Promise.all([
        fetchHistory(selectedConversationId),
        fetchRuntimeTokenUsage(selectedConversationId, 'day'),
        fetchRuntimeContextSnapshot(selectedConversationId)
      ]);
      return {
        personas,
        active,
        activeState,
        activeStateRevision: activeState?.state_revision ?? stableState.state_revision,
        sessions: { ...sessions, active_conversation_id: activeConversationId },
        activeConversationId,
        selectedConversationId,
        history,
        todos: todos.todos,
        tokenUsage,
        contextSnapshot: context.snapshot ?? null
      };
    });
    if (value.activeStateRevision !== state.state_revision) {
      throw new Error('活动角色详情不属于最终稳定 revision。');
    }
    const active = value.activeState
      ? resolveActivePersona(value.activeState, state.active_persona_id)
      : value.active;
    if (value.personas.active_persona_id !== state.active_persona_id) {
      throw new Error('角色列表不属于最终稳定 revision。');
    }
    return {
      stateRevision: state.state_revision,
      personas: value.personas,
      activePersonaId: state.active_persona_id,
      active,
      sessions: value.sessions,
      activeConversationId: value.activeConversationId,
      selectedConversationId: value.selectedConversationId,
      history: value.history,
      todos: value.todos,
      tokenUsage: value.tokenUsage,
      contextSnapshot: value.contextSnapshot
    };
  }

  function applyStoryRuntimeSnapshot(snapshot: StoryRuntimeSnapshot): boolean {
    if (revision.currentStateRevision() !== snapshot.stateRevision) return false;
    const messages = toChatMessages(snapshot.history);
    optionsRef.current.onPersonaBootstrap({
      stateRevision: snapshot.stateRevision,
      personas: snapshot.personas,
      activePersonaId: snapshot.activePersonaId,
      active: snapshot.active
    });
    session.applyRuntimeSessions(
      snapshot.sessions,
      snapshot.activeConversationId,
      snapshot.selectedConversationId
    );
    stream.setMessages(messages);
    overview.setActiveTodos(snapshot.todos);
    overview.setRuntimeTokenUsage(snapshot.tokenUsage);
    overview.setRuntimeContextSnapshot(snapshot.contextSnapshot);
    setDialogue(lastDialogueLine(messages, snapshot.active?.persona ?? null));
    return true;
  }

  async function loadAndCommitStoryRuntimeSnapshot(options: {
    candidateActive?: RevisionBoundActivePersona;
    requestedConversationId?: string | null;
  } = {}): Promise<StoryRuntimeSnapshot> {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const snapshot = await loadStoryRuntimeSnapshot(options);
      if (applyStoryRuntimeSnapshot(snapshot)) return snapshot;
    }
    throw new Error('运行时事实在提交前再次变化，请稍后重试。');
  }

  const stream = useRuntimeStream({
    activePersonaName: options.activePersona?.name,
    conversationId: session.activeConversationId,
    voiceEnabled: voice.voiceEnabled,
    onDialogue: setDialogue,
    onConversationChange: (conversationId) => {
      const normalizedConversationId = normalizeConversationId(conversationId);
      if (normalizedConversationId !== session.activeConversationId) {
        session.applyActiveConversation(normalizedConversationId);
      }
    },
    onRuntimeModeChange: applyRuntimeMode,
    onRuntimeTodosChange: overview.setActiveTodos,
    onRuntimeTokenUsage: (usage: RuntimeTokenUsage) => {
      void overview.refreshRuntimeUsage(usage.conversation_id);
    },
    onRuntimeContextSnapshot: overview.setRuntimeContextSnapshot,
    onSpeech: async (text) => {
      const message = await speak(text, { forceEnabled: true, waitUntilEnded: true });
      if (message) throw new Error(message);
    }
  });

  const follow = useFollowLatest({
    conversationId: session.selectedConversationId,
    updates: stream.messages
  });

  function rejectBlockedMutation(action: string): boolean {
    const reason = optionsRef.current.getMutationBlockReason?.();
    if (!reason) return false;
    stream.setRuntimeStatus(reason);
    optionsRef.current.notify({
      title: `${action}暂不可用`,
      description: reason,
      tone: 'info'
    });
    return true;
  }

  async function bootstrap(isCurrent: () => boolean) {
    optionsRef.current.onPersonaBootstrapStart?.();
    try {
      const requestedConversationId = readConversationIdFromUrl();
      const [model, mode] = await Promise.all([
        fetchModelInfo(),
        fetchRuntimeMode()
      ]);
      let committed = false;
      for (let attempt = 0; attempt < 3 && !committed; attempt += 1) {
        const snapshot = await loadStoryRuntimeSnapshot({ requestedConversationId });
        if (!isCurrent()) return;
        committed = applyStoryRuntimeSnapshot(snapshot);
      }
      if (!committed) {
        throw new Error('初始化快照在提交前已过期，请重试。');
      }
      setModelLabel(formatModelInfoLabel(model));
      applyRuntimeMode(mode.mode, mode.focus_phase, mode.tool_preset);
      stream.setRuntimeStatus('');
    } catch (error) {
      if (!isCurrent()) return;
      optionsRef.current.onPersonaBootstrapError?.(error);
      stream.setRuntimeStatus(error instanceof Error ? error.message : '初始化失败');
      setDialogue({
        speaker: '系统',
        text: '本地运行时暂不可用，请确认后端服务已启动。',
        role: 'system'
      });
    }
  }

  useEffect(() => {
    let current = true;
    void bootstrap(() => current);
    // 冷启动与用户重试共用同一条稳定 revision 读取链路。
    return () => {
      current = false;
    };
  }, [bootstrapAttempt]);

  const retryBootstrap = useCallback(() => {
    setBootstrapAttempt((attempt) => attempt + 1);
  }, []);

  async function synchronizeRuntimeAfterPersonaReset(
    candidateActive: RevisionBoundActivePersona
  ): Promise<StoryRuntimeSnapshot> {
    stream.closeCurrentChatSource();
    stopVoice();
    return loadAndCommitStoryRuntimeSnapshot({ candidateActive });
  }

  async function handleSend() {
    const text = inputValue.trim();
    if (!text || stream.busy) return;
    if (rejectBlockedMutation('发送消息')) return;
    if (session.selectedConversationReadOnly) {
      optionsRef.current.notify({
        title: '当前会话为只读历史',
        description: session.selectedConversationReadOnlyReason,
        tone: 'info'
      });
      return;
    }
    follow.followOutgoingMessage();
    const accepted = await stream.sendMessage(text);
    if (accepted) setInputValue('');
  }

  async function handleReset(): Promise<boolean> {
    if (stream.busy) return false;
    if (rejectBlockedMutation('新建会话')) return false;
    stream.setBusy(true);
    try {
      stream.closeCurrentChatSource();
      stopVoice();
      const response = await resetConversation();
      await loadAndCommitStoryRuntimeSnapshot({
        requestedConversationId: response.conversation_id
      });
      follow.followOutgoingMessage();
      stream.setRuntimeStatus('新对话已开始。');
      return true;
    } catch (err) {
      const message = err instanceof Error ? err.message : '新建会话失败。';
      stream.setRuntimeStatus(message);
      optionsRef.current.notify({
        title: '新建会话失败',
        description: message,
        tone: 'error'
      });
      return false;
    } finally {
      stream.setBusy(false);
    }
  }

  async function handleResumeSession(conversationId?: string): Promise<boolean> {
    if (stream.busy) return false;
    if (rejectBlockedMutation('恢复会话')) return false;
    const target = conversationId
      ? session.runtimeSessions.find((item) => item.conversation_id === conversationId)
      : session.selectedRuntimeSession;
    if (!target?.can_resume || target.conversation_id === session.activeConversationId) return false;
    stream.setBusy(true);
    try {
      stream.closeCurrentChatSource();
      stopVoice();
      const response = await resumeRuntimeSession(target.conversation_id);
      await loadAndCommitStoryRuntimeSnapshot({
        requestedConversationId: response.conversation_id
      });
      stream.setRuntimeStatus(response.status || '会话已恢复。');
      optionsRef.current.notify({
        title: '会话已恢复',
        description: `已载入 ${response.restored_messages} 条上下文消息。`,
        tone: 'success'
      });
      return true;
    } catch (err) {
      const message = err instanceof Error ? err.message : '恢复会话失败。';
      stream.setRuntimeStatus('恢复会话失败。');
      optionsRef.current.notify({ title: '恢复会话失败', description: message, tone: 'error' });
      return false;
    } finally {
      stream.setBusy(false);
    }
  }

  async function handleForkSession(
    conversationId?: string,
    targetPersonaId?: string
  ): Promise<boolean> {
    if (stream.busy) return false;
    if (rejectBlockedMutation('分叉会话')) return false;
    const target = conversationId
      ? session.runtimeSessions.find((item) => item.conversation_id === conversationId)
      : session.selectedRuntimeSession;
    if (!target?.can_resume) {
      optionsRef.current.notify({
        title: '暂无可分叉内容',
        description: '请选择一个已有 transcript 记录的历史会话。',
        tone: 'info'
      });
      return false;
    }
    stream.setBusy(true);
    try {
      stream.closeCurrentChatSource();
      stopVoice();
      const response = await forkRuntimeSession(target.conversation_id, undefined, targetPersonaId);
      await loadAndCommitStoryRuntimeSnapshot({
        requestedConversationId: response.conversation_id
      });
      stream.setRuntimeStatus('会话已分叉。');
      optionsRef.current.notify({
        title: '会话已分叉',
        description:
          response.status ||
          `新会话 ${response.conversation_id} 已载入 ${response.restored_messages} 条上下文消息。`,
        tone: 'success'
      });
      return true;
    } catch (err) {
      const message = err instanceof Error ? err.message : '分叉会话失败。';
      stream.setRuntimeStatus('分叉会话失败。');
      optionsRef.current.notify({ title: '分叉会话失败', description: message, tone: 'error' });
      return false;
    } finally {
      stream.setBusy(false);
    }
  }

  async function handleDeleteRuntimeSession(conversationId: string) {
    if (stream.busy) return;
    if (rejectBlockedMutation('删除会话')) return;
    const targetSession = session.runtimeSessions.find(
      (item) => item.conversation_id === conversationId
    );
    if (!targetSession?.can_resume) {
      optionsRef.current.notify({
        title: '暂无可删除内容',
        description: '空会话没有落盘 transcript，不需要删除。',
        tone: 'info'
      });
      return;
    }
    const title =
      targetSession.summary?.trim() ||
      targetSession.first_prompt?.trim() ||
      conversationId;
    const deletingActive = conversationId === session.activeConversationId;
    const confirmed = window.confirm(
      deletingActive
        ? `确定删除当前会话“${title}”吗？删除后会自动切换到新对话。`
        : `确定删除会话“${title}”吗？这会删除本地 transcript。`
    );
    if (!confirmed) return;
    stream.setBusy(true);
    try {
      if (deletingActive) {
        stream.closeCurrentChatSource();
        stopVoice();
      }
      const response = await deleteRuntimeSession(conversationId);
      const requestedConversationId =
        conversationId === session.selectedConversationId || deletingActive
          ? response.active_conversation_id
          : session.selectedConversationId;
      await loadAndCommitStoryRuntimeSnapshot({ requestedConversationId });
      stream.setRuntimeStatus(response.status);
      optionsRef.current.notify({
        title: '会话已删除',
        description: `已清理 ${response.deleted_records} 条 transcript 记录。`,
        tone: 'success'
      });
    } catch (err) {
      optionsRef.current.notify({
        title: '删除会话失败',
        description: err instanceof Error ? err.message : '删除本地 transcript 时遇到错误。',
        tone: 'error'
      });
    } finally {
      stream.setBusy(false);
    }
  }

  async function handleSelectRuntimeSession(conversationId: string) {
    if (stream.busy) return;
    const targetConversationId = normalizeConversationId(conversationId);
    if (targetConversationId === session.selectedConversationId) {
      session.applySelectedConversation(targetConversationId);
      return;
    }
    const request = session.beginConversationSelection();
    try {
      const { value } = await loadAtStableRevision({
        load: () =>
          Promise.all([
            fetchHistory(targetConversationId, request.controller.signal),
            fetchRuntimeTokenUsage(targetConversationId, 'day', request.controller.signal),
            fetchRuntimeContextSnapshot(targetConversationId, request.controller.signal)
          ]),
        readState: () => fetchRuntimeState(request.controller.signal),
        currentRevision: revision.currentStateRevision,
        acceptRevision: revision.acceptStateRevision
      });
      if (!session.isConversationSelectionCurrent(request)) return;
      session.finishConversationSelection(request);
      session.applySelectedConversation(targetConversationId);
      const [history, tokenUsage, context] = value;
      const messages = toChatMessages(history);
      stream.setMessages(messages);
      overview.setRuntimeTokenUsage(tokenUsage);
      overview.setRuntimeContextSnapshot(context.snapshot ?? null);
      setDialogue(lastDialogueLine(messages, options.activePersona));
    } catch (err) {
      if (request.controller.signal.aborted) return;
      optionsRef.current.notify({
        title: '获取会话历史失败',
        description: err instanceof Error ? err.message : '只读历史查询遇到错误。',
        tone: 'error'
      });
    } finally {
      session.finishConversationSelection(request);
    }
  }

  async function selectSessionFromHistory(conversationId: string) {
    await handleSelectRuntimeSession(conversationId);
    session.setSessionPanelOpen(false);
  }

  function openSessionHistory() {
    session.setSessionPanelOpen(true);
    const selectedSession = session.runtimeSessions.find(
      (item) => item.conversation_id === session.selectedConversationId
    );
    if (selectedSession?.can_resume && selectedSession.records > 0) return;
    const latestSession = session.runtimeSessions.find(
      (item) => item.can_resume && item.records > 0
    );
    if (latestSession && latestSession.conversation_id !== session.selectedConversationId) {
      void handleSelectRuntimeSession(latestSession.conversation_id);
    }
  }

  async function startNewConversationFromHistory(): Promise<boolean> {
    const started = await handleReset();
    if (started) session.setSessionPanelOpen(false);
    return started;
  }

  return {
    modelLabel,
    setModelLabel,
    dialogue,
    inputValue,
    setInputValue,
    runtimeMode,
    runtimeFocusPhase,
    runtimeToolPreset,
    ...session,
    ...overview,
    messages: stream.messages,
    busy: stream.busy,
    setBusy: stream.setBusy,
    canceling: stream.canceling,
    canCancel: stream.canCancel,
    runtimeStatus: stream.runtimeStatus,
    setRuntimeStatus: stream.setRuntimeStatus,
    resolveApproval: stream.resolveApproval,
    resolveUserQuestion: stream.resolveUserQuestion,
    cancelCurrentTurn: stream.cancelCurrentTurn,
    closeCurrentChatSource: stream.closeCurrentChatSource,
    voice,
    audioRef,
    visualizerCanvasRef,
    speak,
    toggleVoice,
    stopVoice,
    startVoiceInput,
    refreshVoiceCapabilities,
    chatScrollRef: follow.scrollRef,
    hasUnreadUpdate: follow.hasUnreadUpdate,
    handleChatScroll: follow.onScroll,
    scrollToLatest: follow.scrollToLatest,
    acceptStateRevision: revision.acceptStateRevision,
    currentStateRevision: revision.currentStateRevision,
    loadStableRuntimeState,
    showActivePersona,
    retryBootstrap,
    synchronizeRuntimeAfterPersonaReset,
    handleSend,
    handleReset,
    handleResumeSession,
    handleForkSession,
    handleDeleteRuntimeSession,
    handleSelectRuntimeSession,
    selectSessionFromHistory,
    openSessionHistory,
    startNewConversationFromHistory
  };
}
