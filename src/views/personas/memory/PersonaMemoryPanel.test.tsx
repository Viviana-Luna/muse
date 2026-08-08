import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import * as api from '@/api';
import type {
  MemoryDetailResponse,
  MemoryHistoryResponse,
  MemoryQueryPageReceipt,
  PersonaLibraryItem
} from '@/types';

import { PersonaMemoryPanel } from './PersonaMemoryPanel';

vi.mock('@/api', () => ({
  adjustPersonaMemoryImportance: vi.fn(),
  clearPersonaMemories: vi.fn(),
  correctPersonaMemory: vi.fn(),
  createPersonaMemory: vi.fn(),
  deletePersonaMemory: vi.fn(),
  fetchPersonaMemory: vi.fn(),
  fetchPersonaMemoryHistory: vi.fn(),
  searchPersonaMemories: vi.fn()
}));

const persona: PersonaLibraryItem = {
  id: 'alice',
  name: '爱丽丝',
  summary: '可靠的同行者',
  default_visual_pack_id: 'visual-alice',
  author: 'Muse',
  version: '1.0.0',
  visual_preview: { avatar_path: null, portrait_path: null }
};

const page: MemoryQueryPageReceipt = {
  items: [
    {
      memory_id: 'memory-1',
      revision_id: 'revision-1',
      category: 'user_preference',
      facet: 'preference_drink',
      keywords: ['绿茶', '饮品偏好'],
      content: '用户喜欢在晚上喝绿茶',
      importance: 'normal',
      event_time: null,
      recorded_at: '2026-08-01T12:00:00Z',
      valid_from: '2026-08-01T12:00:00Z',
      valid_to: null,
      change_type: 'create',
      change_reason: '用户明确告知'
    }
  ],
  has_more: false
};

const detail: MemoryDetailResponse = {
  entry: {
    memory_id: 'memory-1',
    persona_id: 'alice',
    category: 'user_preference',
    current_revision_id: 'revision-1',
    importance: 'normal',
    freshness_at: '2026-08-01T12:00:00Z',
    created_at: '2026-08-01T12:00:00Z',
    state: 'active'
  },
  current_revision: {
    revision_id: 'revision-1',
    memory_id: 'memory-1',
    facet: 'preference_drink',
    keywords: ['绿茶', '饮品偏好'],
    content: '用户喜欢在晚上喝绿茶',
    event_time: null,
    recorded_at: '2026-08-01T12:00:00Z',
    valid_from: '2026-08-01T12:00:00Z',
    valid_to: null,
    change_type: 'create',
    change_reason: '用户明确告知',
    source_conversation_id: 'conversation-1',
    source_turn_id: 'turn-1',
    safety_policy_version: 'memory-safety/v1',
    state: 'current'
  },
  source_conversation_id: 'conversation-1',
  source_turn_id: 'turn-1'
};

const history: MemoryHistoryResponse = {
  memory_id: 'memory-1',
  revisions: [detail.current_revision]
};

function renderPanel() {
  const notify = vi.fn();
  const onBack = vi.fn();
  const onOpenSource = vi.fn();
  render(
    <PersonaMemoryPanel
      persona={persona}
      notify={notify}
      onBack={onBack}
      onOpenSource={onOpenSource}
    />
  );
  return { notify, onBack, onOpenSource };
}

describe('PersonaMemoryPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.searchPersonaMemories).mockResolvedValue(page);
    vi.mocked(api.fetchPersonaMemory).mockResolvedValue(detail);
    vi.mocked(api.fetchPersonaMemoryHistory).mockResolvedValue(history);
    vi.mocked(api.adjustPersonaMemoryImportance).mockResolvedValue({
      operation_id: 'importance-1',
      memory_id: 'memory-1',
      previous_importance: 'normal',
      importance: 'high',
      durable_at: '2026-08-02T00:00:00Z'
    });
    vi.mocked(api.createPersonaMemory).mockResolvedValue({
      operation: 'create',
      memory_id: 'memory-1',
      revision_id: 'revision-1',
      state: 'durable'
    });
    vi.mocked(api.deletePersonaMemory).mockResolvedValue({
      deletion_id: 'deletion-1',
      deleted_memory_count: 1,
      completed_at: '2026-08-02T00:00:00Z'
    });
  });

  afterEach(() => cleanup());

  it('进入页面默认加载记忆列表（浏览模式）', async () => {
    renderPanel();

    await waitFor(() =>
      expect(api.searchPersonaMemories).toHaveBeenCalledWith(
        'alice',
        expect.objectContaining({ query: '', cursor: undefined }),
        expect.anything()
      )
    );
    expect(
      await screen.findByRole('button', { name: /用户喜欢在晚上喝绿茶/ })
    ).toBeVisible();
  });

  it('搜索、读取详情、调整重要程度并打开来源会话', async () => {
    const { onOpenSource } = renderPanel();
    fireEvent.change(screen.getByPlaceholderText('搜索至少 3 个字符'), {
      target: { value: '绿茶偏好' }
    });
    fireEvent.click(screen.getByRole('button', { name: '搜索' }));

    const result = await screen.findByRole('button', { name: /用户喜欢在晚上喝绿茶/ });
    fireEvent.click(result);
    expect(await screen.findByRole('heading', { name: '用户喜欢在晚上喝绿茶' })).toBeVisible();

    fireEvent.change(screen.getByRole('combobox', { name: '调整记忆重要程度' }), {
      target: { value: 'high' }
    });
    await waitFor(() => expect(api.adjustPersonaMemoryImportance).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole('button', { name: '打开来源回合' }));
    expect(onOpenSource).toHaveBeenCalledWith('conversation-1', 'turn-1');
  });

  it('少于 3 个有效字符时本地拒绝搜索（仅保留初始浏览加载）', async () => {
    renderPanel();
    await waitFor(() => expect(api.searchPersonaMemories).toHaveBeenCalledOnce());
    fireEvent.change(screen.getByPlaceholderText('搜索至少 3 个字符'), {
      target: { value: '茶！' }
    });
    fireEvent.click(screen.getByRole('button', { name: '搜索' }));

    expect(screen.getByRole('alert')).toHaveTextContent('请输入至少 3 个有效字符再搜索。');
    expect(api.searchPersonaMemories).toHaveBeenCalledTimes(1);
  });

  it('可以手工新增，并为重试生成稳定 operation_id', async () => {
    const { notify } = renderPanel();
    fireEvent.click(screen.getByRole('button', { name: '新增记忆' }));
    fireEvent.change(screen.getByLabelText('记忆内容'), {
      target: { value: '用户喜欢在晚上喝绿茶' }
    });
    fireEvent.change(screen.getByLabelText('变化原因'), {
      target: { value: '用户明确告知偏好' }
    });
    fireEvent.click(screen.getByRole('button', { name: '保存记忆' }));

    await waitFor(() => expect(api.createPersonaMemory).toHaveBeenCalledOnce());
    expect(api.createPersonaMemory).toHaveBeenCalledWith(
      'alice',
      expect.objectContaining({
        content: '用户喜欢在晚上喝绿茶',
        change_reason: '用户明确告知偏好',
        operation_id: expect.stringMatching(/^memory-create-/u)
      })
    );
    expect(notify).toHaveBeenCalledWith(expect.objectContaining({ title: '记忆已保存' }));
    expect(await screen.findByRole('heading', { name: '用户喜欢在晚上喝绿茶' })).toBeVisible();
  });

  it('永久删除前展示影响说明，确认后从结果中移除', async () => {
    const { notify } = renderPanel();
    fireEvent.change(screen.getByPlaceholderText('搜索至少 3 个字符'), {
      target: { value: '绿茶偏好' }
    });
    fireEvent.click(screen.getByRole('button', { name: '搜索' }));
    fireEvent.click(await screen.findByRole('button', { name: /用户喜欢在晚上喝绿茶/ }));
    await screen.findByRole('heading', { name: '用户喜欢在晚上喝绿茶' });

    fireEvent.click(screen.getByRole('button', { name: '删除' }));
    const dialog = screen.getByRole('alertdialog');
    expect(within(dialog).getByText(/原始聊天仍保留/)).toBeVisible();
    fireEvent.click(within(dialog).getByRole('button', { name: '删除记忆' }));

    await waitFor(() => expect(api.deletePersonaMemory).toHaveBeenCalledOnce());
    expect(notify).toHaveBeenCalledWith(expect.objectContaining({ title: '记忆已永久删除' }));
    expect(screen.queryByRole('heading', { name: '用户喜欢在晚上喝绿茶' })).not.toBeInTheDocument();
  });
});
