import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { DEFAULT_TOAST_DURATIONS, useAppToast } from './useAppToast';

describe('useAppToast', () => {
  it.each([
    ['success', 3200],
    ['info', 4500],
    ['warning', 6000],
    ['error', 8000]
  ] as const)('%s 通知使用对应的默认自动收起时间', (tone, duration) => {
    const { result } = renderHook(() => useAppToast());

    act(() => result.current.notify({ title: '测试通知', tone }));

    expect(result.current.toasts[0]).toMatchObject({
      tone,
      duration: DEFAULT_TOAST_DURATIONS[tone]
    });
    expect(result.current.toasts[0]?.duration).toBe(duration);
  });

  it('调用方可以覆盖默认自动收起时间', () => {
    const { result } = renderHook(() => useAppToast());

    act(() => result.current.notify({ title: '自定义通知', duration: 1200 }));

    expect(result.current.toasts[0]?.duration).toBe(1200);
  });
});
