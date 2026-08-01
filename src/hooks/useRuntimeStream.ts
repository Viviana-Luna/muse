import { useCallback, useEffect, useRef, useState } from 'react';

import {
  answerRuntimeUserQuestion,
  approveRuntimeTool,
  cancelRuntimeTool,
  cancelRuntimeTurn,
  cancelRuntimeUserQuestion,
  streamRuntimeChat
} from '@/api';
import type {
  Message,
  RuntimeContextSnapshot,
  RuntimeApprovalModePreset,
  RuntimeEvent,
  RuntimeTurnStartedEvent,
  RuntimeTodoItem,
  RuntimeTokenUsage,
  RuntimeUserQuestionItem
} from '@/types';

type RuntimeSpeaker = 'system' | 'user' | 'assistant';

export type ChatProcessState =
  | 'active'
  | 'completed'
  | 'error'
  | 'waiting_approval'
  | 'waiting_user';

export type ChatMessageStatus =
  | 'idle'
  | 'queued'
  | 'thinking'
  | 'brief'
  | 'generating'
  | 'tool_running'
  | 'tool_completed'
  | 'approval_pending'
  | 'user_question_pending'
  | 'speech_started'
  | 'speech_finished'
  | 'synthesizing'
  | 'completed'
  | 'cancelled'
  | 'failed';

export interface ChatProcessStep {
  id: string;
  phase: ChatMessageStatus;
  message: string;
  detail?: string;
  state: ChatProcessState;
  time: string;
  approvalId?: string;
  callId?: string;
  toolName?: string;
  risk?: string;
  argumentsPreview?: string;
  approvalRequired?: boolean;
  riskLabel?: string;
  riskTone?: 'safe' | 'warn' | 'danger';
  toolSummary?: Array<{
    label: string;
    value: string;
  }>;
  approvalHint?: string;
  interactionError?: string;
  questionRequestId?: string;
  questions?: RuntimeUserQuestionItem[];
  memoryActivity?: ChatMemoryActivity;
}

export interface ChatMemoryReference {
  memoryId: string;
  revisionId: string;
  category?: string;
  importance?: string;
}

export interface ChatMemoryActivity {
  kind: 'query' | 'staged' | 'commit' | 'delete' | 'failed';
  label: string;
  count?: number;
  hasMore?: boolean;
  references?: ChatMemoryReference[];
  errorCode?: string;
}

export interface ChatMessage extends Message {
  id: string;
  status: ChatMessageStatus;
  process: ChatProcessStep[];
  reasoning?: string;
  streaming?: boolean;
}

export interface RuntimeDialogueLine {
  speaker: string;
  text: string;
  role: RuntimeSpeaker;
  voiceId?: string;
}

interface UseRuntimeStreamOptions {
  activePersonaName?: string;
  conversationId?: string;
  voiceEnabled?: boolean;
  onDialogue: (line: RuntimeDialogueLine) => void;
  onConversationChange?: (conversationId: string) => void;
  onRuntimeModeChange?: (mode: string, focusPhase: string, toolPreset: string) => void;
  onApprovalModeChange?: (preset: RuntimeApprovalModePreset, revision: number) => void;
  onRuntimeTodosChange?: (todos: RuntimeTodoItem[]) => void;
  onRuntimeTokenUsage?: (usage: RuntimeTokenUsage) => void;
  onRuntimeContextSnapshot?: (snapshot: RuntimeContextSnapshot) => void;
  onTurnStarted?: (event: RuntimeTurnStartedEvent) => void;
  onReplyComplete?: (reply: string) => void | Promise<void>;
  onSpeech?: (text: string, voiceId?: string) => void | Promise<void>;
}

function runtimePreferenceSourceLabel(source?: string): string {
  if (source === 'persona_preference') return '角色偏好';
  if (source === 'global_active') return '全局活动配置';
  if (source === 'unavailable') return '不可用';
  return source || '未记录';
}

function turnRuntimeDetail(payload: RuntimeTurnStartedEvent): string | undefined {
  const details: string[] = [];
  if (payload.model) {
    details.push(`模型：${payload.model}（${runtimePreferenceSourceLabel(payload.model_source)}）`);
  }
  if (payload.active_voice_id) {
    details.push(
      `音色：${payload.active_voice_id}（${runtimePreferenceSourceLabel(payload.voice_source)}）`
    );
  } else if (payload.voice_source === 'unavailable') {
    details.push('音色：不可用，本轮仅文本回复');
  }
  if (payload.model_fallback_reason) details.push(`模型回退：${payload.model_fallback_reason}`);
  if (payload.voice_fallback_reason) details.push(`语音说明：${payload.voice_fallback_reason}`);
  return details.length > 0 ? details.join(' · ') : undefined;
}

function createMessageId(prefix: string) {
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function normalizeChatPhase(phase?: string): ChatMessageStatus {
  const known = new Set<ChatMessageStatus>([
    'queued',
    'thinking',
    'brief',
    'generating',
    'tool_running',
    'tool_completed',
    'approval_pending',
    'user_question_pending',
    'speech_started',
    'speech_finished',
    'synthesizing',
    'completed',
    'cancelled',
    'failed'
  ]);
  return known.has(phase as ChatMessageStatus) ? (phase as ChatMessageStatus) : 'thinking';
}

function normalizeProcessState(state?: string): ChatProcessState {
  if (state === 'waiting_approval') return state;
  if (state === 'waiting_user') return state;
  if (state === 'completed' || state === 'error') return state;
  return 'active';
}

export function chatStatusLabel(status: ChatMessageStatus) {
  const labels: Record<ChatMessageStatus, string> = {
    idle: '已记录',
    queued: '已发送',
    thinking: '分析中',
    brief: '阶段简报',
    generating: '生成中',
    tool_running: '工具执行中',
    tool_completed: '工具已完成',
    approval_pending: '等待审批',
    user_question_pending: '等待选择',
    speech_started: '语音播报中',
    speech_finished: '语音已完成',
    synthesizing: '整理中',
    completed: '已完成',
    cancelled: '已停止',
    failed: '失败'
  };
  return labels[status];
}

export function toChatMessages(history: Message[]): ChatMessage[] {
  return history.map((message, index) => ({
    ...message,
    id: `history-${index}-${message.role}`,
    status: 'completed',
    process: []
  }));
}

function formatStepTime() {
  return new Intl.DateTimeFormat('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  }).format(new Date());
}

function formatRuntimeValue(value: unknown) {
  if (value == null) return '';
  if (typeof value === 'string') return value;
  try {
    const text = JSON.stringify(value, null, 2);
    return text.length > 1600 ? `${text.slice(0, 1600)}\n...（参数已截断）` : text;
  } catch {
    return String(value);
  }
}

function runtimeRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function runtimeArgText(argumentsValue: unknown, key: string) {
  const value = runtimeRecord(argumentsValue)[key];
  if (typeof value === 'string') return value.trim();
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return '';
}

function memoryActivityForToolResult(
  toolName: string | undefined,
  success: boolean,
  structuredValue: unknown
): ChatMemoryActivity | undefined {
  if (!toolName?.startsWith('memory_')) return undefined;
  const structured = runtimeRecord(structuredValue);
  if (!success) {
    return {
      kind: 'failed',
      label: '长期记忆操作未完成',
      errorCode: runtimeArgText(structured, 'error_code') || undefined
    };
  }
  if (toolName === 'memory_query') {
    const items = Array.isArray(structured.items) ? structured.items : [];
    const references = items.flatMap((value) => {
      const item = runtimeRecord(value);
      const memoryId = runtimeArgText(item, 'memory_id');
      const revisionId = runtimeArgText(item, 'revision_id');
      if (!memoryId || !revisionId) return [];
      return [{
        memoryId,
        revisionId,
        category: runtimeArgText(item, 'category') || undefined,
        importance: runtimeArgText(item, 'importance') || undefined
      }];
    });
    return {
      kind: 'query',
      label: `本轮读取了 ${references.length} 条相关记忆`,
      count: references.length,
      hasMore: structured.has_more === true,
      references
    };
  }
  if (toolName === 'memory_mutate') {
    return { kind: 'staged', label: '记忆变更已暂存，等待本轮可靠提交' };
  }
  if (toolName === 'memory_delete') {
    const count = typeof structured.deleted_memory_count === 'number'
      ? structured.deleted_memory_count
      : undefined;
    return { kind: 'delete', label: '长期记忆已永久删除', count };
  }
  return undefined;
}

function memoryCommitActivity(payload: RuntimeEvent): ChatMemoryActivity | undefined {
  if (payload.type !== 'status') return undefined;
  if (payload.phase === 'memory_commit_completed') {
    const count = payload.detail?.match(/(\d+)\s*项/u)?.[1];
    return {
      kind: 'commit',
      label: payload.message || '本次记忆已保存',
      count: count ? Number(count) : undefined
    };
  }
  if (payload.phase === 'memory_commit_failed') {
    return {
      kind: 'failed',
      label: payload.message || '本次记忆未保存',
      errorCode: payload.detail || undefined
    };
  }
  return undefined;
}

function normalizeRuntimeTodoItems(value: unknown): RuntimeTodoItem[] | null {
  if (!Array.isArray(value)) return null;
  return value.flatMap((item, index) => {
    const record = runtimeRecord(item);
    const content = typeof record.content === 'string' ? record.content.trim() : '';
    const status = typeof record.status === 'string' ? record.status.trim() : '';
    if (!content || !status) return [];
    const id =
      typeof record.id === 'string' && record.id.trim()
        ? record.id.trim()
        : `todo-${index + 1}`;
    const priority =
      typeof record.priority === 'string' && record.priority.trim()
        ? record.priority.trim()
        : undefined;
    return [{ id, content, status, priority }];
  });
}

function previewText(value: string, max = 180) {
  const normalized = value.replace(/\s+/g, ' ').trim();
  return normalized.length > max ? `${normalized.slice(0, max)}...` : normalized;
}

function riskLabel(risk?: string) {
  const labels: Record<string, string> = {
    read_only: '只读',
    write_file: '写文件',
    execute_command: '执行命令',
    network: '联网',
    external_side_effect: '外部副作用'
  };
  return labels[risk || ''] || risk || '未知风险';
}

function riskTone(risk?: string): ChatProcessStep['riskTone'] {
  if (risk === 'execute_command') return 'danger';
  if (risk === 'write_file' || risk === 'network' || risk === 'external_side_effect') return 'warn';
  return 'safe';
}

function summarizeToolArguments(toolName?: string, argumentsValue?: unknown): ChatProcessStep['toolSummary'] {
  const rows: Array<{ label: string; value: string }> = [];
  const add = (label: string, value: string) => {
    if (value) rows.push({ label, value: previewText(value) });
  };

  switch (toolName) {
    case 'todo_write': {
      const todos = runtimeRecord(argumentsValue).todos;
      if (Array.isArray(todos)) add('任务数量', `${todos.length}`);
      add('更新摘要', runtimeArgText(argumentsValue, 'summary'));
      break;
    }
    case 'enter_plan_mode':
      add('进入原因', runtimeArgText(argumentsValue, 'reason'));
      break;
    case 'exit_plan_mode':
      add('计划摘要', runtimeArgText(argumentsValue, 'plan_summary'));
      add('确认后动作', runtimeArgText(argumentsValue, 'next_action'));
      break;
    case 'send_user_message':
    case 'brief':
      add('简报内容', runtimeArgText(argumentsValue, 'message'));
      add(
        '等待回复',
        toolName === 'brief' ? '否' : runtimeArgText(argumentsValue, 'requires_reply')
      );
      break;
    case 'load_skill':
    case 'use_skill':
    case 'skill':
      add('技能名称', runtimeArgText(argumentsValue, 'skill_name'));
      break;
    case 'agent':
      add('子任务', runtimeArgText(argumentsValue, 'task'));
      add('上下文', runtimeArgText(argumentsValue, 'context'));
      add('期望产出', runtimeArgText(argumentsValue, 'expected_output'));
      add('优先级', runtimeArgText(argumentsValue, 'priority'));
      break;
    case 'task_stop':
      add('任务 ID', runtimeArgText(argumentsValue, 'task_id'));
      add('停止原因', runtimeArgText(argumentsValue, 'reason'));
      break;
    case 'file_read':
      add('读取路径', runtimeArgText(argumentsValue, 'path'));
      break;
    case 'file_list':
      add('目录路径', runtimeArgText(argumentsValue, 'path') || '.');
      break;
    case 'file_write': {
      const content = runtimeArgText(argumentsValue, 'content');
      add('写入路径', runtimeArgText(argumentsValue, 'path'));
      add('写入内容', content ? `${content.length} 字符 · ${previewText(content, 80)}` : '');
      break;
    }
    case 'file_edit':
      add('编辑路径', runtimeArgText(argumentsValue, 'path'));
      add('替换原文', runtimeArgText(argumentsValue, 'old_string'));
      add('替换为', runtimeArgText(argumentsValue, 'new_string'));
      break;
    case 'command_run':
      add('命令', runtimeArgText(argumentsValue, 'command'));
      add('工作目录', runtimeArgText(argumentsValue, 'cwd') || '.');
      add('超时', runtimeArgText(argumentsValue, 'timeout_ms'));
      break;
    case 'web_search':
      add('搜索词', runtimeArgText(argumentsValue, 'query'));
      break;
    case 'web_fetch':
      add('URL', runtimeArgText(argumentsValue, 'url'));
      break;
    case 'persona_switch':
      add('目标角色', runtimeArgText(argumentsValue, 'persona_id'));
      break;
    case 'tts_speak':
      add('朗读文本', runtimeArgText(argumentsValue, 'text'));
      add('调用原因', runtimeArgText(argumentsValue, 'reason'));
      break;
    case 'ask_user_question': {
      const questions = runtimeRecord(argumentsValue).questions;
      if (Array.isArray(questions)) add('问题数量', `${questions.length}`);
      break;
    }
    default:
      return undefined;
  }
  return rows.length ? rows : undefined;
}

export function useRuntimeStream({
  activePersonaName,
  conversationId = 'default',
  voiceEnabled = false,
  onDialogue,
  onConversationChange,
  onRuntimeModeChange,
  onApprovalModeChange,
  onRuntimeTodosChange,
  onRuntimeTokenUsage,
  onRuntimeContextSnapshot,
  onTurnStarted,
  onReplyComplete,
  onSpeech
}: UseRuntimeStreamOptions) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [busy, setBusy] = useState(false);
  const [canceling, setCanceling] = useState(false);
  const [runtimeStatus, setRuntimeStatus] = useState('正在同步本地角色与运行状态。');
  const currentReplyRef = useRef('');
  const activeAssistantMessageIdRef = useRef<string | null>(null);
  const activeTurnIdRef = useRef<string | null>(null);
  const activeTurnVoiceIdRef = useRef<string | undefined>(undefined);
  const cancelRequestedRef = useRef(false);
  const currentChatAbortRef = useRef<AbortController | null>(null);
  const deltaFlushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingDeltaFlushRef = useRef<{
    assistantId: string;
    controller: AbortController;
    assistantChanged: boolean;
    reasoningChunks: string[];
    toolOutputChunks: Array<{ callId: string; label: string; content: string }>;
  } | null>(null);

  const updateChatMessage = useCallback((id: string, updater: (message: ChatMessage) => ChatMessage) => {
    setMessages((value) => value.map((message) => (message.id === id ? updater(message) : message)));
  }, []);

  const appendChatProcess = useCallback(
    (assistantId: string, payload: RuntimeEvent, stepPatch?: Partial<ChatProcessStep>) => {
      const phase = normalizeChatPhase(payload.phase);
      const message = payload.message || chatStatusLabel(phase);
      updateChatMessage(assistantId, (item) => ({
        ...item,
        status: phase,
        process: [
          ...item.process.map((step) =>
            step.state === 'active' ||
            step.state === 'waiting_approval' ||
            step.state === 'waiting_user'
              ? { ...step, state: 'completed' as ChatProcessState }
              : step
          ),
          {
            id: createMessageId('process'),
            phase,
            message,
            detail: payload.detail ?? undefined,
            state: normalizeProcessState(payload.state),
            time: formatStepTime(),
            ...stepPatch
          }
        ]
      }));
      setRuntimeStatus(message);
    },
    [updateChatMessage]
  );

  const closeCurrentChatSource = useCallback(() => {
    currentChatAbortRef.current?.abort();
    currentChatAbortRef.current = null;
    if (deltaFlushTimerRef.current) clearTimeout(deltaFlushTimerRef.current);
    deltaFlushTimerRef.current = null;
    pendingDeltaFlushRef.current = null;
    activeAssistantMessageIdRef.current = null;
    activeTurnIdRef.current = null;
    activeTurnVoiceIdRef.current = undefined;
    cancelRequestedRef.current = false;
    setCanceling(false);
  }, []);

  const flushRuntimeDeltas = useCallback(() => {
    if (deltaFlushTimerRef.current) clearTimeout(deltaFlushTimerRef.current);
    deltaFlushTimerRef.current = null;
    const pending = pendingDeltaFlushRef.current;
    pendingDeltaFlushRef.current = null;
    if (!pending || currentChatAbortRef.current !== pending.controller) return;
    const content = currentReplyRef.current;
    updateChatMessage(pending.assistantId, (message) => ({
      ...message,
      content: pending.assistantChanged ? content : message.content,
      reasoning: pending.reasoningChunks.length
        ? `${message.reasoning || ''}${pending.reasoningChunks.join('')}`
        : message.reasoning,
      status: pending.assistantChanged
        ? message.status === 'queued' || message.status === 'thinking'
          ? 'generating'
          : message.status
        : pending.reasoningChunks.length && message.status === 'queued'
          ? 'thinking'
          : pending.toolOutputChunks.length && message.status === 'queued'
            ? 'tool_running'
            : message.status,
      process: pending.toolOutputChunks.length
        ? message.process.map((step) => {
            const chunks = pending.toolOutputChunks.filter(
              (chunk) => chunk.callId === step.callId
            );
            if (chunks.length === 0) return step;
            let detail = step.detail || '';
            for (const chunk of chunks) {
              const prefix = detail.trim() ? `${detail}\n` : '';
              detail = `${prefix}[${chunk.label}] ${chunk.content}`.slice(-4000);
            }
            return {
              ...step,
              detail,
              state: step.state === 'completed' ? 'completed' : 'active'
            };
          })
        : message.process
    }));
    if (pending.assistantChanged) {
      onDialogue({
        speaker: activePersonaName || '当前角色',
        text: content || '......',
        role: 'assistant',
        voiceId: activeTurnVoiceIdRef.current
      });
    }
  }, [activePersonaName, onDialogue, updateChatMessage]);

  const queueRuntimeDelta = useCallback(
    (
      assistantId: string,
      controller: AbortController,
      append: (pending: NonNullable<typeof pendingDeltaFlushRef.current>) => void
    ) => {
      let pending = pendingDeltaFlushRef.current;
      if (
        !pending ||
        pending.assistantId !== assistantId ||
        pending.controller !== controller
      ) {
        flushRuntimeDeltas();
        pending = {
          assistantId,
          controller,
          assistantChanged: false,
          reasoningChunks: [],
          toolOutputChunks: []
        };
        pendingDeltaFlushRef.current = pending;
      }
      append(pending);
      if (deltaFlushTimerRef.current) return;
      deltaFlushTimerRef.current = setTimeout(flushRuntimeDeltas, 32);
    },
    [flushRuntimeDeltas]
  );

  const finishReply = useCallback(
    (controller: AbortController, assistantId: string) => {
      flushRuntimeDeltas();
      controller.abort();
      if (currentChatAbortRef.current === controller) currentChatAbortRef.current = null;
      activeAssistantMessageIdRef.current = null;
      activeTurnIdRef.current = null;
      cancelRequestedRef.current = false;
      const reply = currentReplyRef.current.trim();
      updateChatMessage(assistantId, (message) => ({
        ...message,
        content: reply || message.content || '（本轮没有返回文本）',
        status: 'completed',
        streaming: false,
        process: message.process.map((step) =>
          step.state === 'active' ? { ...step, state: 'completed' } : step
        )
      }));
      if (reply) {
        void onReplyComplete?.(reply);
      }
      setBusy(false);
      setCanceling(false);
      setRuntimeStatus('当前会话已更新。');
      currentReplyRef.current = '';
    },
    [flushRuntimeDeltas, onReplyComplete, updateChatMessage]
  );

  const finishCancelledReply = useCallback(
    (controller: AbortController, assistantId: string, message = '当前回复已停止。') => {
      if (currentChatAbortRef.current !== controller) return;
      flushRuntimeDeltas();
      controller.abort();
      currentChatAbortRef.current = null;
      activeAssistantMessageIdRef.current = null;
      activeTurnIdRef.current = null;
      cancelRequestedRef.current = false;
      const reply = currentReplyRef.current.trim();
      updateChatMessage(assistantId, (item) => ({
        ...item,
        content: reply || item.content || '（本轮已停止）',
        status: 'cancelled',
        streaming: false,
        process: item.process.map((step) =>
          step.state === 'active' ||
          step.state === 'waiting_approval' ||
          step.state === 'waiting_user'
            ? { ...step, state: 'completed' as ChatProcessState }
            : step
        )
      }));
      currentReplyRef.current = '';
      setBusy(false);
      setCanceling(false);
      setRuntimeStatus(message);
    },
    [flushRuntimeDeltas, updateChatMessage]
  );

  const failReply = useCallback(
    (controller: AbortController, assistantId: string, message: string) => {
      if (currentChatAbortRef.current !== controller) return;
      flushRuntimeDeltas();
      controller.abort();
      currentChatAbortRef.current = null;
      activeAssistantMessageIdRef.current = null;
      activeTurnIdRef.current = null;
      cancelRequestedRef.current = false;
      setBusy(false);
      setCanceling(false);
      updateChatMessage(assistantId, (item) => ({
        ...item,
        content: item.content || message,
        status: 'failed',
        streaming: false,
        process: item.process.map((step) =>
          step.state === 'active' || step.state === 'waiting_approval' || step.state === 'waiting_user'
            ? { ...step, state: 'error' as ChatProcessState }
            : step
        )
      }));
      setRuntimeStatus(message);
      onDialogue({ speaker: '系统', text: message, role: 'system' });
      currentReplyRef.current = '';
    },
    [flushRuntimeDeltas, onDialogue, updateChatMessage]
  );

  const sendMessage = useCallback(
    async (rawText: string, selectedSkill?: string): Promise<boolean> => {
      const text = rawText.trim();
      if (!text || busy) return false;
      setBusy(true);
      cancelRequestedRef.current = false;
      currentReplyRef.current = '';
      const userMessage: ChatMessage = {
        id: createMessageId('user'),
        role: 'user',
        content: text,
        status: 'completed',
        process: []
      };
      const assistantId = createMessageId('assistant');
      const assistantMessage: ChatMessage = {
        id: assistantId,
        role: 'assistant',
        content: '',
        reasoning: '',
        status: 'queued',
        process: [
          {
            id: createMessageId('process'),
            phase: 'queued',
            message: '消息已送达，正在等待运行时接手。',
            state: 'active',
            time: formatStepTime()
          }
        ],
        streaming: true
      };
      closeCurrentChatSource();
      activeAssistantMessageIdRef.current = assistantId;
      setMessages((value) => [...value, userMessage, assistantMessage]);
      onDialogue({ speaker: '你', text, role: 'user' });
      setRuntimeStatus('消息已发送，正在建立流式连接。');

      const controller = new AbortController();
      currentChatAbortRef.current = controller;
      const handleEvent = (payload: RuntimeEvent) => {
        if (currentChatAbortRef.current !== controller) return;
        const currentAssistantId = activeAssistantMessageIdRef.current;
        if (!currentAssistantId) return;
        const highFrequencyDelta =
          payload.type === 'assistant_delta' ||
          payload.type === 'reasoning_delta' ||
          payload.type === 'tool_output_delta';
        if (!highFrequencyDelta) flushRuntimeDeltas();
        if (payload.type === 'turn_started') {
          onTurnStarted?.(payload);
          activeTurnIdRef.current = payload.turn_id ?? null;
          activeTurnVoiceIdRef.current = payload.active_voice_id ?? undefined;
          if (payload.conversation_id) {
            onConversationChange?.(payload.conversation_id);
          }
          if (payload.runtime_mode && payload.focus_phase && payload.tool_preset) {
            onRuntimeModeChange?.(
              payload.runtime_mode,
              payload.focus_phase,
              payload.tool_preset
            );
          }
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'queued',
            message: '运行时已创建本轮对话快照。',
            detail: turnRuntimeDetail(payload),
            state: 'active'
          });
        }
        if (payload.type === 'status') {
          appendChatProcess(currentAssistantId, payload, {
            memoryActivity: memoryCommitActivity(payload)
          });
        }
        if (payload.type === 'reasoning_delta') {
          const chunk = payload.content ?? '';
          if (chunk) {
            queueRuntimeDelta(currentAssistantId, controller, (pending) => {
              pending.reasoningChunks.push(chunk);
            });
          }
        }
        if (payload.type === 'assistant_segment_started') {
          if (currentReplyRef.current.trim()) {
            currentReplyRef.current = `${currentReplyRef.current.replace(/\s+$/u, '')}\n\n`;
            updateChatMessage(currentAssistantId, (message) => ({
              ...message,
              content: currentReplyRef.current,
              status: message.status === 'queued' || message.status === 'thinking' ? 'generating' : message.status
            }));
            onDialogue({
              speaker: activePersonaName || '当前角色',
              text: currentReplyRef.current,
              role: 'assistant',
              voiceId: activeTurnVoiceIdRef.current
            });
          }
        }
        if (payload.type === 'assistant_delta') {
          const chunk = payload.content ?? '';
          currentReplyRef.current += chunk;
          if (chunk) {
            queueRuntimeDelta(currentAssistantId, controller, (pending) => {
              pending.assistantChanged = true;
            });
          }
        }
        if (payload.type === 'assistant_message') {
          const content = payload.content ?? '';
          currentReplyRef.current = content;
          updateChatMessage(currentAssistantId, (message) => ({
            ...message,
            content,
            status: 'completed'
          }));
          onDialogue({
            speaker: activePersonaName || '当前角色',
            text: content || '......',
            role: 'assistant',
            voiceId: activeTurnVoiceIdRef.current
          });
        }
        if (payload.type === 'tool_call') {
          const risk = payload.risk;
          const requiresApproval = Boolean(payload.requires_approval);
          const interruptText =
            payload.interrupt_behavior === 'cancel' ? '可随本轮停止' : '停止时等待收尾';
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'tool_running',
            message: `正在调用工具：${payload.name || '未知工具'}`,
            detail: `风险：${riskLabel(risk)}${requiresApproval ? ' · 需要审批' : ' · 自动执行'} · ${interruptText}`,
            state: 'active'
          }, {
            callId: payload.call_id,
            toolName: payload.name,
            risk,
            argumentsPreview: formatRuntimeValue(payload.arguments),
            approvalRequired: requiresApproval,
            riskLabel: riskLabel(risk),
            riskTone: riskTone(risk),
            toolSummary: summarizeToolArguments(payload.name, payload.arguments)
          });
        }
        if (payload.type === 'tool_result') {
          const structured = runtimeRecord(payload.structured);
          const modeState = runtimeRecord(structured.runtime_mode_state);
          const mode = modeState.mode;
          const focusPhase = modeState.focus_phase;
          const toolPreset = modeState.tool_preset;
          if (typeof mode === 'string' && typeof focusPhase === 'string' && typeof toolPreset === 'string') {
            onRuntimeModeChange?.(mode, focusPhase, toolPreset);
          }
          const todos = normalizeRuntimeTodoItems(structured.todos);
          if (todos) onRuntimeTodosChange?.(todos);
          const memoryActivity = memoryActivityForToolResult(
            payload.name,
            payload.success === true,
            payload.structured
          );
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: payload.success ? 'tool_completed' : 'failed',
            message: memoryActivity?.label
              || (payload.success ? '工具调用已完成。' : '工具调用失败，请查看回复。'),
            detail: payload.content || (payload.name ? `工具：${payload.name}` : undefined),
            state: payload.success ? 'completed' : 'error'
          }, {
            callId: payload.call_id,
            toolName: payload.name,
            memoryActivity
          });
        }
        if (payload.type === 'tool_output_delta') {
          const callId = payload.call_id;
          const content = payload.content || '';
          if (callId && content) {
            queueRuntimeDelta(currentAssistantId, controller, (pending) => {
              pending.toolOutputChunks.push({
                callId,
                label: payload.stream === 'stderr' ? 'stderr' : 'stdout',
                content
              });
            });
          }
        }
        if (payload.type === 'token_usage' && payload.usage) {
          onRuntimeTokenUsage?.(payload.usage);
        }
        if (payload.type === 'context_snapshot' && payload.snapshot) {
          onRuntimeContextSnapshot?.(payload.snapshot);
        }
        if (payload.type === 'approval_pending') {
          const risk = payload.risk;
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'approval_pending',
            message: payload.message || '等待用户审批。',
            detail: payload.detail ?? payload.reason ?? formatRuntimeValue(payload.arguments),
            state: payload.state || 'waiting_approval'
          }, {
            approvalId: payload.approval_id,
            callId: payload.call_id,
            toolName: payload.name,
            risk,
            argumentsPreview: formatRuntimeValue(payload.arguments),
            approvalRequired: true,
            riskLabel: riskLabel(risk),
            riskTone: riskTone(risk),
            toolSummary: summarizeToolArguments(payload.name, payload.arguments),
            approvalHint: payload.detail ?? payload.reason ?? undefined
          });
        }
        if (payload.type === 'approval_review_started') {
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'thinking',
            message: 'AUTO 审查器正在评估工具风险。',
            detail: payload.name ? `工具：${payload.name}` : undefined,
            state: 'active'
          });
        }
        if (
          payload.type === 'approval_review_completed' ||
          payload.type === 'approval_review_denied' ||
          payload.type === 'approval_review_timed_out' ||
          payload.type === 'approval_review_aborted'
        ) {
          const allowed = payload.type === 'approval_review_completed';
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: allowed ? 'tool_running' : 'thinking',
            message: allowed
              ? 'AUTO 审查已允许，继续执行工具。'
              : payload.type === 'approval_review_denied'
                ? 'AUTO 审查未放行，等待人工复核。'
                : 'AUTO 审查异常，已安全转为人工审批。',
            detail: payload.rationale ?? payload.reason ?? undefined,
            state: 'completed'
          });
        }
        if (payload.type === 'approval_mode_changed') {
          onApprovalModeChange?.(payload.preset, payload.revision);
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'thinking',
            message: '当前会话已回退为手动审批。',
            detail: payload.reason,
            state: 'completed'
          });
        }
        if (payload.type === 'approval_resolved') {
          const cancelled = !payload.approved && payload.reason?.includes('取消');
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'tool_running',
            message: payload.approved
              ? '审批已允许，继续执行工具。'
              : cancelled
                ? '审批已取消，工具不会执行。'
                : '审批已拒绝，工具不会执行。',
            detail: payload.reason ?? payload.approval_id,
            state: 'completed'
          });
        }
        if (payload.type === 'user_question_pending') {
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'user_question_pending',
            message: payload.message || '等待你选择后继续。',
            detail: payload.detail ?? undefined,
            state: payload.state || 'waiting_user'
          }, {
            questionRequestId: payload.request_id,
            callId: payload.call_id,
            toolName: payload.name,
            questions: Array.isArray(payload.questions) ? payload.questions : undefined,
            argumentsPreview: formatRuntimeValue(payload.arguments),
            approvalRequired: false,
            riskLabel: '用户输入',
            riskTone: 'safe'
          });
        }
        if (payload.type === 'user_question_resolved') {
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'tool_running',
            message: payload.answered ? '已收到你的选择，继续执行。' : '已取消回答，模型会基于现有信息继续。',
            detail: payload.reason ? `原因：${payload.reason}` : undefined,
            state: 'completed'
          });
        }
        if (payload.type === 'speech_started') {
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'speech_started',
            message: payload.message || '开始语音播报。',
            detail: payload.voice_id ? `音色：${payload.voice_id}` : undefined,
            state: 'active'
          }, {
            callId: payload.call_id
          });
          if (payload.text) {
            void Promise.resolve(onSpeech?.(payload.text, payload.voice_id ?? undefined))
              .then(() => {
                appendChatProcess(currentAssistantId, {
                  type: 'status',
                  phase: 'speech_finished',
                  message: '语音播放已完成。',
                  state: 'completed'
                });
              })
              .catch((err) => {
                appendChatProcess(currentAssistantId, {
                  type: 'status',
                  phase: 'failed',
                  message: '语音播报失败。',
                  detail: err instanceof Error ? err.message : String(err),
                  state: 'error'
                });
              });
          }
        }
        if (payload.type === 'speech_finished') {
          appendChatProcess(currentAssistantId, {
            type: 'status',
            phase: 'speech_finished',
            message: payload.message || '语音播报请求已完成。',
            detail: payload.detail ?? undefined,
            state: payload.success === false ? 'error' : 'completed'
          }, {
            callId: payload.call_id
          });
        }
        if (payload.type === 'done') {
          if (cancelRequestedRef.current) {
            finishCancelledReply(controller, currentAssistantId);
          } else {
            finishReply(controller, currentAssistantId);
          }
        }
        if (payload.type === 'error') {
          const message = payload.content || payload.message || '对话失败';
          if (cancelRequestedRef.current) {
            finishCancelledReply(controller, currentAssistantId, '当前回复已停止。');
          } else {
            failReply(controller, currentAssistantId, message);
          }
        }
      };
      void streamRuntimeChat(
        {
          message: text,
          conversation_id: conversationId,
          client_request_id:
            typeof crypto.randomUUID === 'function'
              ? crypto.randomUUID()
              : createMessageId('request'),
          voice_enabled: voiceEnabled,
          ...(selectedSkill ? { selected_skill: selectedSkill } : {})
        },
        { signal: controller.signal, onEvent: handleEvent }
      )
        .then(() => {
          if (currentChatAbortRef.current === controller) {
            failReply(controller, assistantId, '流式连接提前结束，请稍后重试。');
          }
        })
        .catch((err) => {
          if (controller.signal.aborted || currentChatAbortRef.current !== controller) return;
          failReply(
            controller,
            assistantId,
            err instanceof Error ? err.message : '连接中断，请稍后重试。'
          );
        });
      return true;
    },
    [
      activePersonaName,
      appendChatProcess,
      busy,
      conversationId,
      closeCurrentChatSource,
      failReply,
      finishCancelledReply,
      finishReply,
      flushRuntimeDeltas,
      onDialogue,
      onConversationChange,
      onApprovalModeChange,
      onRuntimeModeChange,
      onRuntimeContextSnapshot,
      onTurnStarted,
      onRuntimeTodosChange,
      onRuntimeTokenUsage,
      onSpeech,
      queueRuntimeDelta,
      updateChatMessage,
      voiceEnabled
    ]
  );

  const resolveApproval = useCallback(
    async (approvalId: string, approved: boolean) => {
      const turnId = activeTurnIdRef.current;
      if (!turnId) {
        setRuntimeStatus('审批提交失败：当前回合已结束。');
        return;
      }
      const currentAssistantId = activeAssistantMessageIdRef.current;
      if (currentAssistantId) {
        updateChatMessage(currentAssistantId, (item) => ({
          ...item,
          process: item.process.map((step) =>
            step.approvalId === approvalId
              ? {
                  ...step,
                  interactionError: undefined
                }
              : step
          )
        }));
      }
      try {
        if (approved) {
          await approveRuntimeTool(turnId, approvalId);
        } else {
          await cancelRuntimeTool(turnId, approvalId);
        }
        if (currentAssistantId) {
          updateChatMessage(currentAssistantId, (item) => ({
            ...item,
            process: item.process.map((step) =>
              step.approvalId === approvalId && step.state === 'waiting_approval'
                ? {
                    ...step,
                    state: 'active',
                    detail: approved ? '已允许，等待工具继续执行。' : '已取消，工具不会执行。'
                  }
                : step
            )
          }));
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (currentAssistantId) {
          updateChatMessage(currentAssistantId, (item) => ({
            ...item,
            process: item.process.map((step) =>
              step.approvalId === approvalId
                ? { ...step, state: 'waiting_approval', interactionError: `提交失败：${message}` }
                : step
            )
          }));
        }
        setRuntimeStatus(`审批提交失败：${message}`);
      }
    },
    [updateChatMessage]
  );

  const resolveUserQuestion = useCallback(
    async (
      requestId: string,
      answers?: Record<string, string | string[]>,
      annotations?: Record<string, { notes?: string }>
    ) => {
      const turnId = activeTurnIdRef.current;
      if (!turnId) {
        setRuntimeStatus('答案提交失败：当前回合已结束。');
        return;
      }
      const currentAssistantId = activeAssistantMessageIdRef.current;
      if (currentAssistantId) {
        updateChatMessage(currentAssistantId, (item) => ({
          ...item,
          process: item.process.map((step) =>
            step.questionRequestId === requestId
              ? {
                  ...step,
                  interactionError: undefined
                }
              : step
          )
        }));
      }
      try {
        if (answers) {
          await answerRuntimeUserQuestion(turnId, requestId, { answers, annotations });
        } else {
          await cancelRuntimeUserQuestion(turnId, requestId);
        }
        if (currentAssistantId) {
          updateChatMessage(currentAssistantId, (item) => ({
            ...item,
            process: item.process.map((step) =>
              step.questionRequestId === requestId && step.state === 'waiting_user'
                ? {
                    ...step,
                    state: 'active',
                    detail: answers ? '已提交选择，等待模型继续。' : '已取消回答，等待模型继续。'
                  }
                : step
            )
          }));
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (currentAssistantId) {
          updateChatMessage(currentAssistantId, (item) => ({
            ...item,
            process: item.process.map((step) =>
              step.questionRequestId === requestId
                ? { ...step, state: 'waiting_user', interactionError: `提交失败：${message}` }
                : step
            )
          }));
        }
        setRuntimeStatus(`提交选择失败：${message}`);
      }
    },
    [updateChatMessage]
  );

  const cancelCurrentTurn = useCallback(async () => {
    const turnId = activeTurnIdRef.current;
    if (!turnId || !busy || canceling) return;
    const currentAssistantId = activeAssistantMessageIdRef.current;
    setCanceling(true);
    cancelRequestedRef.current = true;
    if (currentAssistantId) {
      appendChatProcess(currentAssistantId, {
        type: 'status',
        phase: 'thinking',
        message: '正在停止当前回复。',
        state: 'active'
      });
    }
    setRuntimeStatus('正在停止当前回复。');
    try {
      await cancelRuntimeTurn(turnId);
      // 后端终态可能先于取消接口响应到达；此时 SSE 已完成回滚，不能再把卡片改回 active。
      if (!cancelRequestedRef.current || activeTurnIdRef.current !== turnId) return;
      if (currentAssistantId) {
        appendChatProcess(currentAssistantId, {
          type: 'status',
          phase: 'thinking',
          message: '停止请求已提交，正在等待运行时完成回滚。',
          state: 'active'
        });
      }
      setRuntimeStatus('停止请求已提交，正在等待运行时终态。');
    } catch (err) {
      const message = err instanceof Error ? err.message : '停止当前 turn 失败。';
      cancelRequestedRef.current = false;
      setCanceling(false);
      setRuntimeStatus(message);
      if (currentAssistantId) {
        appendChatProcess(currentAssistantId, {
          type: 'status',
          phase: 'failed',
          message: '停止当前 turn 失败。',
          detail: message,
          state: 'error'
        });
      }
    }
  }, [appendChatProcess, busy, canceling, updateChatMessage]);

  useEffect(() => {
    return () => {
      closeCurrentChatSource();
    };
  }, [closeCurrentChatSource]);

  return {
    messages,
    setMessages,
    busy,
    setBusy,
    canceling,
    canCancel: busy && !!activeTurnIdRef.current,
    runtimeStatus,
    setRuntimeStatus,
    sendMessage,
    resolveApproval,
    resolveUserQuestion,
    cancelCurrentTurn,
    closeCurrentChatSource
  };
}
