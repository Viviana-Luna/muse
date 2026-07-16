// 运行时概览状态，统一管理任务清单、Token 用量和上下文快照。

import { useCallback, useEffect, useRef, useState } from 'react';

import { fetchRuntimeContextSnapshot, fetchRuntimeTokenUsage } from '@/api';
import type {
  RuntimeContextSnapshot,
  RuntimeTodoItem,
  RuntimeTokenUsageResponse
} from '@/types';

function normalizeConversationId(conversationId?: string | null) {
  return conversationId?.trim() || 'default';
}

export function useRuntimeOverview() {
  const [activeTodos, setActiveTodos] = useState<RuntimeTodoItem[]>([]);
  const [runtimeTokenUsage, setRuntimeTokenUsage] =
    useState<RuntimeTokenUsageResponse | null>(null);
  const [runtimeContextSnapshot, setRuntimeContextSnapshot] =
    useState<RuntimeContextSnapshot | null>(null);
  const requestRef = useRef<{ id: number; controller: AbortController } | null>(null);

  const refreshRuntimeUsage = useCallback(async (conversationId: string) => {
    const nextConversationId = normalizeConversationId(conversationId);
    requestRef.current?.controller.abort();
    const request = {
      id: (requestRef.current?.id ?? 0) + 1,
      controller: new AbortController()
    };
    requestRef.current = request;
    try {
      const [usage, snapshot] = await Promise.all([
        fetchRuntimeTokenUsage(nextConversationId, 'day', request.controller.signal),
        fetchRuntimeContextSnapshot(nextConversationId, request.controller.signal)
      ]);
      if (requestRef.current !== request || request.controller.signal.aborted) return;
      setRuntimeTokenUsage(usage);
      setRuntimeContextSnapshot(snapshot.snapshot ?? null);
    } catch (err) {
      if (request.controller.signal.aborted || requestRef.current !== request) return;
      setRuntimeTokenUsage(null);
      setRuntimeContextSnapshot(null);
      console.warn('读取运行时 Token 用量或上下文快照失败。', err);
    }
  }, []);

  useEffect(() => () => requestRef.current?.controller.abort(), []);

  return {
    activeTodos,
    setActiveTodos,
    runtimeTokenUsage,
    setRuntimeTokenUsage,
    runtimeContextSnapshot,
    setRuntimeContextSnapshot,
    refreshRuntimeUsage
  };
}
