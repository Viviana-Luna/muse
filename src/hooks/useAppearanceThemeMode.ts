import { useEffect, useState } from 'react';

import type { AppearanceTheme, ResolvedAppearanceTheme } from '@/types';

const LIGHT_THEME_MEDIA_QUERY = '(prefers-color-scheme: light)';

function readSystemThemeMode(): ResolvedAppearanceTheme {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return 'dark';
  }
  return window.matchMedia(LIGHT_THEME_MEDIA_QUERY).matches ? 'light' : 'dark';
}

export function resolveAppearanceThemeMode(
  preference: AppearanceTheme,
  systemTheme: ResolvedAppearanceTheme
): ResolvedAppearanceTheme {
  return preference === 'system' ? systemTheme : preference;
}

/** 将持久化的主题偏好解析为当前界面主题，并响应系统配色实时变化。 */
export function useAppearanceThemeMode(preference: AppearanceTheme): ResolvedAppearanceTheme {
  const [systemTheme, setSystemTheme] = useState<ResolvedAppearanceTheme>(readSystemThemeMode);

  useEffect(() => {
    if (
      preference !== 'system' ||
      typeof window === 'undefined' ||
      typeof window.matchMedia !== 'function'
    ) return;
    const mediaQuery = window.matchMedia(LIGHT_THEME_MEDIA_QUERY);
    const updateSystemTheme = () => setSystemTheme(mediaQuery.matches ? 'light' : 'dark');
    updateSystemTheme();
    mediaQuery.addEventListener('change', updateSystemTheme);
    return () => mediaQuery.removeEventListener('change', updateSystemTheme);
  }, [preference]);

  return resolveAppearanceThemeMode(preference, systemTheme);
}
