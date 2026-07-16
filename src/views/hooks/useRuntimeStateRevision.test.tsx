import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { loadAtStableRevision, useRuntimeStateRevision } from './useRuntimeStateRevision';

describe('useRuntimeStateRevision', () => {
  it('只接受不低于当前值的 revision，阻止跨域旧响应回写', () => {
    const { result } = renderHook(() => useRuntimeStateRevision());

    act(() => {
      expect(result.current.acceptStateRevision(8)).toBe(true);
    });
    expect(result.current.stateRevision).toBe(8);

    act(() => {
      expect(result.current.acceptStateRevision(7)).toBe(false);
    });
    expect(result.current.stateRevision).toBe(8);
    expect(result.current.isStateRevisionCurrent(7)).toBe(false);
    expect(result.current.isStateRevisionCurrent(9)).toBe(true);
  });

  it('状态在加载途中变化时丢弃旧结果并自动重试', async () => {
    const states = [
      { state_revision: 4 },
      { state_revision: 5 },
      { state_revision: 5 },
      { state_revision: 5 }
    ];
    const values = ['旧会话', '新会话'];
    const loadedRevisions: number[] = [];
    let accepted = 0;

    const result = await loadAtStableRevision({
      load: async (stableState) => {
        loadedRevisions.push(stableState.state_revision);
        return values.shift() ?? '缺失';
      },
      readState: async () => states.shift() ?? { state_revision: 5 },
      currentRevision: () => accepted,
      acceptRevision: (revision) => {
        if (revision < accepted) return false;
        accepted = revision;
        return true;
      }
    });

    expect(result.value).toBe('新会话');
    expect(result.state.state_revision).toBe(5);
    expect(accepted).toBe(5);
    expect(loadedRevisions).toEqual([4, 5]);
  });
});
