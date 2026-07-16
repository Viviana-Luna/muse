import { describe, expect, it, vi } from 'vitest';

import type { RuntimeEvent } from '@/types';

import { consumeSseBuffer } from './runtimeStream';

describe('consumeSseBuffer', () => {
  it('支持跨网络分片拼接标准 SSE JSON 事件', () => {
    const events: RuntimeEvent[] = [];
    let buffer = consumeSseBuffer('data: {"type":"assistant_del', (event) => events.push(event));
    buffer = consumeSseBuffer(`${buffer}ta","content":"你好"}\n\ndata: {"type":"done"}\n\n`, (event) => events.push(event));

    expect(buffer).toBe('');
    expect(events).toEqual([
      { type: 'assistant_delta', content: '你好' },
      { type: 'done' }
    ]);
  });

  it('兼容 DONE sentinel，并拒绝损坏的 JSON 事件', () => {
    const onEvent = vi.fn();
    consumeSseBuffer('data: [DONE]\n\n', onEvent);
    expect(onEvent).toHaveBeenCalledWith({ type: 'done' });

    expect(() => consumeSseBuffer('data: {bad json}\n\n', onEvent)).toThrow(
      '运行时返回了无法解析的流式事件。'
    );
  });
});
