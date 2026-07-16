import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import * as api from '@/api';

import { SessionsPage } from './SessionsPage';

vi.mock('@/api', () => ({
  fetchHistory: vi.fn(),
  fetchRuntimeSessions: vi.fn(),
  updateRuntimeSessionMetadata: vi.fn(),
  fetchRuntimeSessionContext: vi.fn(),
  exportRuntimeSession: vi.fn()
}));

function sessionRuntime() {
  return {
    runtimeSessions: [
      {
        conversation_id: 'current',
        summary: '当前计划',
        exists: true,
        can_resume: true,
        records: 6,
        last_time: '2026-07-12T12:00:00Z'
      },
      {
        conversation_id: 'history',
        summary: '雨夜散步',
        exists: true,
        can_resume: true,
        records: 4,
        last_time: '2026-07-11T12:00:00Z'
      }
    ],
    selectedConversationId: 'history',
    activeConversationId: 'current',
    selectedRuntimeSession: {
      conversation_id: 'history',
      summary: '雨夜散步',
      exists: true,
      can_resume: true,
      records: 4
    },
    busy: false,
    chatScrollRef: { current: null },
    handleSelectRuntimeSession: vi.fn().mockResolvedValue(undefined),
    handleResumeSession: vi.fn().mockResolvedValue(true),
    handleForkSession: vi.fn().mockResolvedValue(true),
    handleDeleteRuntimeSession: vi.fn().mockResolvedValue(undefined),
    startNewConversationFromHistory: vi.fn().mockResolvedValue(true),
    applyRuntimeSessions: vi.fn(),
    setRuntimeStatus: vi.fn()
  };
}

describe('SessionsPage', () => {
  afterEach(() => cleanup());

  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.fetchHistory).mockImplementation(async (conversationId) => [
      {
        role: 'user',
        content: conversationId === 'history' ? '继续散步吧' : '检查当前计划'
      }
    ]);
    vi.mocked(api.fetchRuntimeSessions).mockResolvedValue({
      sessions: sessionRuntime().runtimeSessions,
      active_conversation_id: 'current',
      status: 'ok'
    });
    vi.mocked(api.updateRuntimeSessionMetadata).mockResolvedValue({
      conversation_id: 'history',
      title: '雨夜续章',
      archived: false,
      source_conversation_id: null,
      updated_at: '2026-07-13T00:00:00Z',
      revision: 1
    });
    vi.mocked(api.fetchRuntimeSessionContext).mockResolvedValue({
      conversation_id: 'history',
      context_snapshot: null,
      runtime_policy_snapshot: {
        provider: 'deepseek',
        model: 'deepseek-chat',
        persona_version: '1.0.0',
        policy_version: 'persona-resource-policy/v1',
        tool_ids: ['skill']
      },
      status: 'ok'
    });
  });

  it('在独立页面展示会话列表、只读预览和继续操作', async () => {
    const runtime = sessionRuntime();
    const onOpenChat = vi.fn();
    render(
      <SessionsPage runtime={runtime as never} activePersonaName="洛希" onOpenChat={onOpenChat} />
    );

    expect(screen.getByRole('region', { name: '会话管理' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: /查看会话 雨夜散步/ }));
    expect(await screen.findByText('继续散步吧')).toBeVisible();
    expect(runtime.handleSelectRuntimeSession).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: '恢复并继续' }));
    await waitFor(() => expect(runtime.handleResumeSession).toHaveBeenCalledWith('history'));
    expect(onOpenChat).toHaveBeenCalledOnce();
  });

  it('支持搜索、选择、分叉、删除和开始新对话', async () => {
    const runtime = sessionRuntime();
    const onOpenChat = vi.fn();
    render(
      <SessionsPage runtime={runtime as never} activePersonaName="洛希" onOpenChat={onOpenChat} />
    );

    fireEvent.change(screen.getByPlaceholderText('搜索标题、开场或会话 ID'), {
      target: { value: '当前' }
    });
    const list = within(screen.getByRole('complementary', { name: '会话列表' }));
    expect(list.getByText('当前计划')).toBeVisible();
    expect(list.queryByText('雨夜散步')).not.toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText('搜索标题、开场或会话 ID'), {
      target: { value: '' }
    });
    fireEvent.click(list.getByRole('button', { name: /查看会话 雨夜散步/ }));
    await screen.findByText('继续散步吧');

    fireEvent.click(screen.getByRole('button', { name: '分叉后继续' }));
    await waitFor(() => expect(runtime.handleForkSession).toHaveBeenCalledWith('history'));

    fireEvent.click(screen.getByRole('button', { name: '删除会话' }));
    expect(runtime.handleDeleteRuntimeSession).toHaveBeenCalledWith('history');

    fireEvent.click(screen.getByRole('button', { name: '开始新对话' }));
    await waitFor(() => expect(runtime.startNewConversationFromHistory).toHaveBeenCalledOnce());
  });

  it('支持重命名、归档与只读上下文检查', async () => {
    const runtime = sessionRuntime();
    render(
      <SessionsPage runtime={runtime as never} activePersonaName="洛希" onOpenChat={vi.fn()} />
    );

    fireEvent.click(screen.getByRole('button', { name: /查看会话 雨夜散步/ }));
    await screen.findByText('继续散步吧');
    fireEvent.click(screen.getByRole('button', { name: '重命名会话' }));
    fireEvent.change(screen.getByRole('textbox', { name: '会话标题' }), {
      target: { value: '雨夜续章' }
    });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(api.updateRuntimeSessionMetadata).toHaveBeenCalledWith('history', {
        title: '雨夜续章'
      })
    );

    fireEvent.click(screen.getByRole('button', { name: '上下文' }));
    expect(await screen.findByText('deepseek / deepseek-chat')).toBeVisible();
    expect(screen.getByText('这里展示回合开始时冻结的事实，不允许编辑。')).toBeVisible();

    fireEvent.click(screen.getByRole('button', { name: '归档' }));
    await waitFor(() =>
      expect(api.updateRuntimeSessionMetadata).toHaveBeenCalledWith('history', {
        archived: true
      })
    );
    expect(runtime.applyRuntimeSessions).toHaveBeenCalled();
  });
});
