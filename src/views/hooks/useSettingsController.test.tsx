import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  fetchAppearancePreferences,
  fetchDiagnosticsConnectivity,
  saveAppearancePreferences
} from '@/api';
import type { DiagnosticsConnectivityItem } from '@/types';
import { useSettingsController } from './useSettingsController';
import { useSettingsDraft } from './useSettingsDraft';

vi.mock('@/api', () => ({
  fetchDiagnosticsConnectivity: vi.fn(),
  fetchAppearancePreferences: vi.fn(),
  fetchModelCatalog: vi.fn(),
  fetchModelInfo: vi.fn(),
  fetchModelsConfig: vi.fn(),
  fetchRuntimeWorkspaces: vi.fn(),
  fetchWebSearchConfig: vi.fn(),
  saveModelsConfig: vi.fn(),
  saveAppearancePreferences: vi.fn(),
  saveWebSearchConfig: vi.fn(),
  updateRuntimeWorkspacePolicy: vi.fn()
}));

vi.mock('./useProviderCatalogController', () => ({
  normalizeSettingsConfig: vi.fn((config) => config),
  useProviderCatalogController: vi.fn(() => ({}))
}));

const checks: DiagnosticsConnectivityItem[] = [
  {
    id: 'chat',
    label: '对话模型 API',
    target: 'https://api.example.com/v1',
    status: 'reachable',
    latency_ms: 79,
    message: '连接正常。'
  },
  {
    id: 'tts',
    label: 'TTS API',
    target: '未配置',
    status: 'skipped',
    latency_ms: null,
    message: '未配置 API Base。'
  },
  {
    id: 'asr',
    label: '语音识别 API',
    target: '未配置',
    status: 'skipped',
    latency_ms: null,
    message: '未启用。'
  }
];

function renderSettingsController() {
  const notify = vi.fn();
  const setBusy = vi.fn();
  const hook = renderHook(() => {
    const draft = useSettingsDraft();
    const controller = useSettingsController({
      draft,
      notify,
      setBusy,
      speak: vi.fn(async () => null),
      refreshVoiceCapabilities: vi.fn(async () => undefined),
      setModelLabel: vi.fn(),
      navigateToStory: vi.fn()
    });
    return { draft, controller };
  });
  return { ...hook, notify, setBusy };
}

describe('useSettingsController 诊断反馈', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();
  });

  it('检测成功后更新持久结果，并通过通知反馈完成数量', async () => {
    vi.mocked(fetchDiagnosticsConnectivity).mockResolvedValue({ checks });
    const { result, notify, setBusy } = renderSettingsController();

    await act(async () => result.current.controller.handleRefreshDiagnostics());

    expect(result.current.draft.diagnosticsChecks).toEqual(checks);
    expect(notify).toHaveBeenCalledWith({
      title: '连通性检测完成',
      description: '已完成 3 项检测。',
      tone: 'success'
    });
    expect(setBusy).toHaveBeenNthCalledWith(1, true);
    expect(setBusy).toHaveBeenLastCalledWith(false);
  });

  it('检测失败只通过错误通知反馈，不覆盖已有检测结果', async () => {
    vi.mocked(fetchDiagnosticsConnectivity).mockRejectedValue(new Error('诊断服务不可用'));
    const { result, notify, setBusy } = renderSettingsController();

    act(() => result.current.draft.setDiagnosticsChecks(checks));
    await act(async () => result.current.controller.handleRefreshDiagnostics());

    expect(result.current.draft.diagnosticsChecks).toEqual(checks);
    expect(notify).toHaveBeenCalledWith({
      title: '连通性检测失败',
      description: '诊断服务不可用',
      tone: 'error'
    });
    expect(setBusy).toHaveBeenLastCalledWith(false);
  });

  it('启动时将旧 localStorage 外观一次性导入 config.toml', async () => {
    window.localStorage.setItem(
      'agent-vp:appearance-settings',
      JSON.stringify({ backgroundBlur: 9, backgroundOpacity: 0.7, motionLevel: 'reduced' })
    );
    vi.mocked(fetchAppearancePreferences).mockResolvedValue({
      schema_version: 1,
      appearance: {
        theme: 'system',
        background_theme: 'dark',
        language: 'zh-CN',
        background_blur: 18,
        background_opacity: 1,
        motion_level: 'full'
      },
      diagnostics: []
    });
    vi.mocked(saveAppearancePreferences).mockResolvedValue({
      schema_version: 1,
      appearance: {
        theme: 'system',
        background_theme: 'dark',
        language: 'zh-CN',
        background_blur: 9,
        background_opacity: 0.7,
        motion_level: 'reduced'
      },
      diagnostics: []
    });
    const { result } = renderSettingsController();

    await act(async () => result.current.controller.initializeAppearancePreferences());

    expect(saveAppearancePreferences).toHaveBeenCalledWith({
      theme: 'system',
      background_theme: 'dark',
      background_blur: 9,
      background_opacity: 0.7,
      motion_level: 'reduced'
    });
    expect(result.current.draft.appearanceSettings).toEqual({
      theme: 'system',
      backgroundTheme: 'dark',
      backgroundBlur: 9,
      backgroundOpacity: 0.7,
      motionLevel: 'reduced'
    });
    expect(window.localStorage.getItem('agent-vp:appearance-settings')).toBeNull();
  });

  it('已有 config.toml 外观优先于旧 localStorage 且保留字段诊断', async () => {
    window.localStorage.setItem(
      'muse:appearance-settings',
      JSON.stringify({ backgroundBlur: 5, backgroundOpacity: 0.5, motionLevel: 'none' })
    );
    vi.mocked(fetchAppearancePreferences).mockResolvedValue({
      schema_version: 1,
      appearance: {
        theme: 'dark',
        background_theme: 'light',
        language: 'zh-CN',
        background_blur: 24,
        background_opacity: 0.8,
        motion_level: 'reduced'
      },
      diagnostics: [
        {
          code: 'config_unknown_field',
          field_path: 'appearance.future_field',
          message: '该字段会原样保留。'
        }
      ]
    });
    const { result, notify } = renderSettingsController();

    await act(async () => result.current.controller.initializeAppearancePreferences());

    expect(saveAppearancePreferences).not.toHaveBeenCalled();
    expect(result.current.draft.appearanceSettings.theme).toBe('dark');
    expect(result.current.draft.appearanceSettings.backgroundTheme).toBe('light');
    expect(result.current.draft.appearanceSettings.backgroundBlur).toBe(24);
    expect(window.localStorage.getItem('muse:appearance-settings')).toBeNull();
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({
        title: '用户配置包含兼容诊断',
        tone: 'warning'
      })
    );
  });
});
