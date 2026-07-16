import { useEffect, useMemo, useRef, useState } from 'react';
import type { Dispatch, SetStateAction } from 'react';

import type { RuntimeSessionItem, RuntimeSessionListResponse } from '@/types';

const CONVERSATION_QUERY_KEY = 'conversation_id';

export function normalizeConversationId(conversationId?: string | null): string {
  return conversationId?.trim() || 'default';
}

export function readConversationIdFromUrl(): string | null {
  if (typeof window === 'undefined') return null;
  const value = new URLSearchParams(window.location.search).get(CONVERSATION_QUERY_KEY);
  return value?.trim() || null;
}

function writeConversationIdToUrl(conversationId: string) {
  if (typeof window === 'undefined') return;
  const nextId = normalizeConversationId(conversationId);
  const url = new URL(window.location.href);
  if (url.searchParams.get(CONVERSATION_QUERY_KEY) === nextId) return;
  url.searchParams.set(CONVERSATION_QUERY_KEY, nextId);
  window.history.replaceState(window.history.state, '', `${url.pathname}${url.search}${url.hash}`);
}

export function resolveSelectedConversationId(
  requestedConversationId: string | null | undefined,
  sessions: RuntimeSessionItem[],
  activeConversationId: string
): string {
  const requestedId = requestedConversationId?.trim();
  if (!requestedId) return activeConversationId;
  const exists =
    requestedId === activeConversationId ||
    sessions.some((session) => session.conversation_id === requestedId);
  return exists ? requestedId : activeConversationId;
}

export interface ConversationSelectionRequest {
  controller: AbortController;
  requestId: number;
}

export interface AppliedSessionSelection {
  activeConversationId: string;
  selectedConversationId: string;
}

export interface UseSessionStateResult {
  sessionPanelOpen: boolean;
  setSessionPanelOpen: Dispatch<SetStateAction<boolean>>;
  activeConversationId: string;
  selectedConversationId: string;
  runtimeSessions: RuntimeSessionItem[];
  selectedRuntimeSession: RuntimeSessionItem | undefined;
  selectedConversationReadOnly: boolean;
  selectedConversationReadOnlyReason: string;
  applySelectedConversation: (conversationId: string) => string;
  applyActiveConversation: (conversationId: string) => string;
  applyRuntimeSessions: (
    sessions: RuntimeSessionListResponse,
    nextActiveConversationId?: string,
    nextSelectedConversationId?: string
  ) => AppliedSessionSelection;
  beginConversationSelection: () => ConversationSelectionRequest;
  isConversationSelectionCurrent: (request: ConversationSelectionRequest) => boolean;
  finishConversationSelection: (request: ConversationSelectionRequest) => void;
}

export function useSessionState(): UseSessionStateResult {
  const [sessionPanelOpen, setSessionPanelOpen] = useState(false);
  const [activeConversationId, setActiveConversationId] = useState('default');
  const [selectedConversationId, setSelectedConversationId] = useState('default');
  const [runtimeSessions, setRuntimeSessions] = useState<RuntimeSessionItem[]>([]);
  const conversationSelectionAbortRef = useRef<AbortController | null>(null);
  const conversationSelectionRequestRef = useRef(0);

  const selectedRuntimeSession = useMemo(
    () => runtimeSessions.find((session) => session.conversation_id === selectedConversationId),
    [selectedConversationId, runtimeSessions]
  );
  const selectedConversationReadOnly = selectedConversationId !== activeConversationId;
  const selectedConversationReadOnlyReason = selectedConversationReadOnly
    ? '正在查看历史会话，不能直接续写；请先恢复该会话或从此分叉。'
    : '';

  useEffect(
    () => () => {
      conversationSelectionAbortRef.current?.abort();
    },
    []
  );

  function applySelectedConversation(conversationId: string): string {
    conversationSelectionAbortRef.current?.abort();
    conversationSelectionAbortRef.current = null;
    conversationSelectionRequestRef.current += 1;
    const nextId = normalizeConversationId(conversationId);
    setSelectedConversationId(nextId);
    writeConversationIdToUrl(nextId);
    return nextId;
  }

  function applyActiveConversation(conversationId: string): string {
    const nextId = normalizeConversationId(conversationId);
    setActiveConversationId(nextId);
    applySelectedConversation(nextId);
    return nextId;
  }

  function applyRuntimeSessions(
    sessions: RuntimeSessionListResponse,
    nextActiveConversationId?: string,
    nextSelectedConversationId?: string
  ): AppliedSessionSelection {
    const nextActiveId = normalizeConversationId(
      nextActiveConversationId || sessions.active_conversation_id
    );
    const nextSelectedId = resolveSelectedConversationId(
      nextSelectedConversationId || nextActiveId,
      sessions.sessions,
      nextActiveId
    );
    setRuntimeSessions(sessions.sessions);
    setActiveConversationId(nextActiveId);
    applySelectedConversation(nextSelectedId);
    return {
      activeConversationId: nextActiveId,
      selectedConversationId: nextSelectedId
    };
  }

  function beginConversationSelection(): ConversationSelectionRequest {
    conversationSelectionAbortRef.current?.abort();
    const controller = new AbortController();
    conversationSelectionAbortRef.current = controller;
    const requestId = conversationSelectionRequestRef.current + 1;
    conversationSelectionRequestRef.current = requestId;
    return { controller, requestId };
  }

  function isConversationSelectionCurrent(request: ConversationSelectionRequest): boolean {
    return (
      !request.controller.signal.aborted &&
      conversationSelectionRequestRef.current === request.requestId &&
      conversationSelectionAbortRef.current === request.controller
    );
  }

  function finishConversationSelection(request: ConversationSelectionRequest) {
    if (conversationSelectionAbortRef.current === request.controller) {
      conversationSelectionAbortRef.current = null;
    }
  }

  return {
    sessionPanelOpen,
    setSessionPanelOpen,
    activeConversationId,
    selectedConversationId,
    runtimeSessions,
    selectedRuntimeSession,
    selectedConversationReadOnly,
    selectedConversationReadOnlyReason,
    applySelectedConversation,
    applyActiveConversation,
    applyRuntimeSessions,
    beginConversationSelection,
    isConversationSelectionCurrent,
    finishConversationSelection
  };
}
