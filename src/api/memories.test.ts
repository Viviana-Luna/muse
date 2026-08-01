import { beforeEach, describe, expect, it, vi } from 'vitest';

const client = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  readJson: vi.fn()
}));

vi.mock('./client', () => client);

import {
  adjustPersonaMemoryImportance,
  clearPersonaMemories,
  correctPersonaMemory,
  createPersonaMemory,
  deletePersonaMemory,
  fetchPersonaMemory,
  fetchPersonaMemoryHistory,
  searchPersonaMemories
} from './memories';

describe('长期记忆 API', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    client.apiFetch.mockResolvedValue(new Response('{}'));
    client.readJson.mockResolvedValue({});
  });

  it('搜索条件和不透明游标只进入角色作用域 URL', async () => {
    await searchPersonaMemories('角色/甲', {
      query: '绿茶 偏好',
      category: 'user_preference',
      importance: 'high',
      cursor: '下一页/中文'
    });

    expect(client.apiFetch).toHaveBeenCalledWith(
      '/api/personas/%E8%A7%92%E8%89%B2%2F%E7%94%B2/memories?query=%E7%BB%BF%E8%8C%B6+%E5%81%8F%E5%A5%BD&category=user_preference&importance=high&cursor=%E4%B8%8B%E4%B8%80%E9%A1%B5%2F%E4%B8%AD%E6%96%87',
      { signal: undefined }
    );
  });

  it('详情与历史接口编码 memory_id', async () => {
    await fetchPersonaMemory('alice', 'memory/1');
    await fetchPersonaMemoryHistory('alice', 'memory/1');

    expect(client.apiFetch).toHaveBeenNthCalledWith(
      1,
      '/api/personas/alice/memories/memory%2F1',
      { signal: undefined }
    );
    expect(client.apiFetch).toHaveBeenNthCalledWith(
      2,
      '/api/personas/alice/memories/memory%2F1/history',
      { signal: undefined }
    );
  });

  it('所有管理写入保持后端要求的方法和 operation_id', async () => {
    await createPersonaMemory('alice', {
      category: 'user_fact',
      content: '用户住在苏州',
      importance: 'normal',
      event_time: null,
      change_reason: '用户明确告知',
      operation_id: 'create-1'
    });
    await correctPersonaMemory('alice', 'memory-1', {
      expected_revision_id: 'revision-1',
      category: 'user_fact',
      content: '用户住在常州',
      event_time: null,
      change_reason: '用户纠正',
      operation_id: 'correct-1'
    });
    await adjustPersonaMemoryImportance('alice', 'memory-1', {
      expected_revision_id: 'revision-2',
      expected_importance: 'normal',
      importance: 'high',
      operation_id: 'importance-1'
    });
    await deletePersonaMemory('alice', 'memory-1', 'delete-1');
    await clearPersonaMemories('alice', 'clear-1');

    const calls = client.apiFetch.mock.calls as Array<[string, RequestInit]>;
    expect(calls.map(([url, init]) => [url, init.method])).toEqual([
      ['/api/personas/alice/memories', 'POST'],
      ['/api/personas/alice/memories/memory-1/correct', 'POST'],
      ['/api/personas/alice/memories/memory-1/importance', 'POST'],
      ['/api/personas/alice/memories/memory-1', 'DELETE'],
      ['/api/personas/alice/memories', 'DELETE']
    ]);
    expect(JSON.parse(String(calls[3]?.[1].body))).toEqual({ operation_id: 'delete-1' });
    expect(JSON.parse(String(calls[4]?.[1].body))).toEqual({ operation_id: 'clear-1' });
  });
});
