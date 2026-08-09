import { act, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { resolveAppearanceThemeMode, useAppearanceThemeMode } from './useAppearanceThemeMode';

const originalMatchMedia = window.matchMedia;

afterEach(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: originalMatchMedia
  });
});

describe('useAppearanceThemeMode', () => {
  it('显式深色或浅色优先于系统主题', () => {
    expect(resolveAppearanceThemeMode('dark', 'light')).toBe('dark');
    expect(resolveAppearanceThemeMode('light', 'dark')).toBe('light');
  });

  it('跟随系统时响应配色变化', () => {
    let matches = false;
    let changeListener: (() => void) | undefined;
    const mediaQuery = {
      get matches() {
        return matches;
      },
      media: '(prefers-color-scheme: light)',
      onchange: null,
      addEventListener: vi.fn((_type: string, listener: () => void) => {
        changeListener = listener;
      }),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(() => true)
    } as unknown as MediaQueryList;
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn(() => mediaQuery)
    });

    const { result } = renderHook(() => useAppearanceThemeMode('system'));
    expect(result.current).toBe('dark');

    act(() => {
      matches = true;
      changeListener?.();
    });
    expect(result.current).toBe('light');
  });
});
