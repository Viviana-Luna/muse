import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';

import {
  readConversationIdFromUrl,
  resolveSelectedConversationId,
  useSessionState
} from './useSessionState';
import type { RuntimeSessionItem, RuntimeSessionListResponse } from '@/types';

const sessions: RuntimeSessionItem[] = [
  {
    conversation_id: 'active',
    exists: true,
    can_resume: true,
    records: 2
  },
  {
    conversation_id: 'history',
    exists: true,
    can_resume: true,
    records: 5
  }
];

const response: RuntimeSessionListResponse = {
  sessions,
  active_conversation_id: 'active',
  status: 'ok'
};

describe('useSessionState', () => {
  beforeEach(() => {
    window.history.replaceState({}, '', '/');
  });

  it('只允许选择当前列表中的会话并同步 URL', () => {
    expect(resolveSelectedConversationId('missing', sessions, 'active')).toBe('active');
    expect(resolveSelectedConversationId('history', sessions, 'active')).toBe('history');

    const { result } = renderHook(() => useSessionState());
    act(() => result.current.applyRuntimeSessions(response, 'active', 'history'));

    expect(result.current.activeConversationId).toBe('active');
    expect(result.current.selectedConversationId).toBe('history');
    expect(result.current.selectedConversationReadOnly).toBe(true);
    expect(result.current.selectedRuntimeSession?.records).toBe(5);
    expect(readConversationIdFromUrl()).toBe('history');
  });

  it('新请求会取消旧请求，并通过代次阻止过期响应', () => {
    const { result } = renderHook(() => useSessionState());
    let first!: ReturnType<typeof result.current.beginConversationSelection>;
    let second!: ReturnType<typeof result.current.beginConversationSelection>;

    act(() => {
      first = result.current.beginConversationSelection();
      second = result.current.beginConversationSelection();
    });

    expect(first.controller.signal.aborted).toBe(true);
    expect(result.current.isConversationSelectionCurrent(first)).toBe(false);
    expect(result.current.isConversationSelectionCurrent(second)).toBe(true);

    act(() => result.current.finishConversationSelection(second));
    expect(result.current.isConversationSelectionCurrent(second)).toBe(false);
  });
});
