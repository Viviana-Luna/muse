import type { AppearanceBackgroundTheme, AppearanceTheme } from '@/types';
import { apiFetch, readJson } from './client';

export type AppearanceMotionLevel = 'full' | 'reduced' | 'none';

export interface ConfigDiagnostic {
  code: string;
  field_path: string;
  message: string;
}

export interface AppearancePreferencesResponse {
  schema_version: number;
  appearance: {
    theme: AppearanceTheme;
    background_theme: AppearanceBackgroundTheme;
    language: string;
    background_blur: number;
    background_opacity: number;
    motion_level: AppearanceMotionLevel;
  };
  diagnostics: ConfigDiagnostic[];
}

export interface AppearancePreferencesUpdate {
  theme: AppearanceTheme;
  background_theme: AppearanceBackgroundTheme;
  background_blur: number;
  background_opacity: number;
  motion_level: AppearanceMotionLevel;
}

/** 读取由用户级 config.toml 持久化的外观偏好。 */
export async function fetchAppearancePreferences(): Promise<AppearancePreferencesResponse> {
  return readJson<AppearancePreferencesResponse>(
    await apiFetch('/api/preferences/appearance')
  );
}

/** 只更新界面当前可编辑的外观字段，后端保留 TOML 中的其他字段和注释。 */
export async function saveAppearancePreferences(
  update: AppearancePreferencesUpdate
): Promise<AppearancePreferencesResponse> {
  return readJson<AppearancePreferencesResponse>(
    await apiFetch('/api/preferences/appearance', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(update)
    })
  );
}
