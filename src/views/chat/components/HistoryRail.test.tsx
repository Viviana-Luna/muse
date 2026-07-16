import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { HistoryRail } from './HistoryRail';

afterEach(cleanup);

function renderHistoryRail(overrides: Partial<ComponentProps<typeof HistoryRail>> = {}) {
  const onSelectSession = vi.fn();
  const onDeleteSession = vi.fn();
  const onResumeSession = vi.fn();
  const onForkSession = vi.fn();
  const onNewSession = vi.fn();

  render(
    <HistoryRail
      sessions={[
        {
          conversation_id: 'active',
          summary: '新对话',
          exists: false,
          can_resume: false,
          records: 0
        },
        {
          conversation_id: 'history-1',
          summary: '历史问候',
          exists: true,
          can_resume: true,
          records: 4,
          last_time: '2026-07-10T09:00:00Z'
        }
      ]}
      activeConversationId="active"
      selectedConversationId="history-1"
      onSelectSession={onSelectSession}
      onDeleteSession={onDeleteSession}
      onResumeSession={onResumeSession}
      onForkSession={onForkSession}
      onClose={vi.fn()}
      onNewSession={onNewSession}
      {...overrides}
    />
  );

  return { onDeleteSession, onForkSession, onNewSession, onResumeSession, onSelectSession };
}

describe('HistoryRail', () => {
  it('只呈现历史会话列表，并由选中项提供分叉和更多操作', () => {
    const { onDeleteSession, onForkSession, onResumeSession, onSelectSession } = renderHistoryRail();
    const expectedTime = new Intl.DateTimeFormat('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit'
    }).format(new Date('2026-07-10T09:00:00Z'));

    expect(screen.getByRole('complementary', { name: '历史会话列表' })).toBeVisible();
    expect(screen.queryByRole('button', { name: '关闭会话列表' })).not.toBeInTheDocument();
    expect(screen.queryByRole('region', { name: '历史会话记录' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '新对话 · 当前 暂无记录' })).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole('button', {
        name: `查看会话 历史问候，4 条记录 · ${expectedTime}`
      })
    );
    expect(onSelectSession).toHaveBeenCalledWith('history-1');

    fireEvent.click(screen.getByRole('button', { name: '恢复会话' }));
    expect(onResumeSession).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByRole('button', { name: '从此分叉' }));
    expect(onForkSession).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByLabelText('更多会话操作'));
    fireEvent.click(screen.getByRole('button', { name: '删除会话' }));
    expect(onDeleteSession).toHaveBeenCalledWith('history-1');
  });

  it('没有可恢复记录时只显示历史空状态', () => {
    const { onNewSession } = renderHistoryRail({
      sessions: [],
      selectedConversationId: 'active'
    });

    expect(screen.getByText('暂无历史会话')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '开始新对话' }));
    expect(onNewSession).toHaveBeenCalledOnce();
  });
});
