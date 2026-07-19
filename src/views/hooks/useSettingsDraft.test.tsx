import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';

import { useSettingsDraft } from './useSettingsDraft';
import { normalizeSettingsConfig } from './useProviderCatalogController';
import type { ModelCatalog, ModelsConfig } from '@/types';

const config: ModelsConfig = {
  chat: {
    provider: 'deepseek',
    api_base: 'https://api.deepseek.com',
    api_key: '••••••••',
    api_key_configured: true,
    model: 'deepseek-chat'
  },
  tts: {
    enabled: false,
    provider: 'openai_audio_speech',
    api_base: '',
    api_key: null,
    api_key_configured: false,
    model: '',
    voice_id: '',
    speed: 1,
    response_format: 'mp3'
  },
  speech_recognition: {
    enabled: true,
    provider: 'openai_audio_transcriptions',
    api_base: 'https://api.openai.com/v1',
    api_key: null,
    api_key_configured: false,
    model: 'whisper-1',
    language: 'zh',
    response_format: 'json'
  },
  audio_understanding: {
    provider: '',
    api_base: '',
    api_key: null,
    api_key_configured: false,
    model: ''
  },
  voice_input: { mode: 'speech_text' }
};

const catalog: ModelCatalog = {
  providers: [],
  models: [],
  capabilities: []
};

describe('useSettingsDraft', () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it('加载设置时不把密钥掩码写入输入框值', () => {
    const normalized = normalizeSettingsConfig(config, catalog);

    expect(normalized.chat.api_key).toBeNull();
    expect(normalized.chat.api_key_configured).toBe(true);
    expect(normalized.tts.api_key).toBeNull();
  });

  it('一次应用后端事实状态并保留密钥占位信息', () => {
    const { result } = renderHook(() => useSettingsDraft());

    act(() =>
      result.current.applyLoadedSettings({
        config,
        catalog,
        permissionMode: 'deny',
        sandboxMode: 'read_only',
        webSearchProvider: 'exa_api',
        webSearchConfigured: true,
        panel: 'web_search'
      })
    );

    expect(result.current.settingsOpen).toBe(true);
    expect(result.current.settingsPanel).toBe('web_search');
    expect(result.current.settingsConfig?.chat.model).toBe('deepseek-chat');
    expect(result.current.maskedKeys.chat).toBe('••••••••');
    expect(result.current.workspacePermissionMode).toBe('deny');
    expect(result.current.webSearchProvider).toBe('exa_api');
    expect(result.current.webSearchConfigured).toBe(true);
  });

  it('对模型、ASR 与 Web Search 草稿执行局部原子更新', () => {
    const { result } = renderHook(() => useSettingsDraft());
    act(() =>
      result.current.applyLoadedSettings({
        config,
        catalog,
        permissionMode: 'request_approval',
        sandboxMode: 'workspace_write',
        webSearchProvider: 'exa_free_mcp',
        webSearchConfigured: false,
        panel: 'chat'
      })
    );

    act(() => {
      result.current.updateSection('chat', 'model', 'deepseek-reasoner');
      result.current.updateSpeechRecognition('model', 'gpt-4o-mini-transcribe');
      result.current.updateSpeechRecognition('language', 'ja');
      result.current.stageWebSearchProvider('exa_api');
      result.current.stageWebSearchKey('exa-draft');
    });

    expect(result.current.settingsConfig?.chat.model).toBe('deepseek-reasoner');
    expect(result.current.settingsConfig?.speech_recognition.model).toBe('gpt-4o-mini-transcribe');
    expect(result.current.settingsConfig?.speech_recognition.language).toBe('ja');
    expect(result.current.settingsConfig?.voice_input.mode).toBe('speech_text');
    expect(result.current.webSearchConfigured).toBe(false);
    expect(result.current.webSearchProvider).toBe('exa_api');
    expect(result.current.webSearchKeyDraft).toBe('exa-draft');
    expect(result.current.dirtyDomains).toEqual(expect.arrayContaining(['models', 'web_search']));
  });

  it('外观草稿不再写 localStorage，后端提交后只更新内存事实快照', () => {
    const { result } = renderHook(() => useSettingsDraft());

    act(() => {
      result.current.setAppearanceSettings((current) => ({
        ...current,
        backgroundBlur: current.backgroundBlur + 1
      }));
    });

    expect(window.localStorage.getItem('muse:appearance-settings')).toBeNull();
    expect(result.current.dirtyDomains).toContain('appearance');

    act(() => result.current.markAppearanceSaved());
    expect(window.localStorage.getItem('muse:appearance-settings')).toBeNull();
    expect(result.current.dirtyDomains).not.toContain('appearance');
  });

  it('读取旧外观键作为一次性导入候选，后端确认后清理新旧键', () => {
    window.localStorage.setItem(
      'agent-vp:appearance-settings',
      JSON.stringify({ backgroundBlur: 9, backgroundOpacity: 0.7, motionLevel: 'reduced' })
    );
    const { result } = renderHook(() => useSettingsDraft());

    expect(result.current.appearanceSettings.backgroundBlur).toBe(9);
    expect(result.current.legacyAppearanceSettings?.backgroundBlur).toBe(9);
    act(() => result.current.markAppearanceSaved());
    expect(window.localStorage.getItem('muse:appearance-settings')).toBeNull();
    expect(window.localStorage.getItem('agent-vp:appearance-settings')).toBeNull();
    expect(result.current.legacyAppearanceSettings).toBeNull();
  });

  it('部分保存成功只清除成功领域，失败领域草稿继续保留', () => {
    const { result } = renderHook(() => useSettingsDraft());
    act(() =>
      result.current.applyLoadedSettings({
        config,
        catalog,
        permissionMode: 'request_approval',
        sandboxMode: 'workspace_write',
        webSearchProvider: 'exa_api',
        webSearchConfigured: true,
        panel: 'chat'
      })
    );
    act(() => {
      result.current.updateSection('chat', 'model', 'next-model');
      result.current.stageWebSearchDelete();
    });
    expect(result.current.dirtyDomains).toEqual(expect.arrayContaining(['models', 'web_search']));

    act(() => result.current.markModelsSaved(result.current.settingsConfig!));
    expect(result.current.dirtyDomains).not.toContain('models');
    expect(result.current.dirtyDomains).toContain('web_search');
    expect(result.current.webSearchAction).toBe('delete');
    expect(result.current.webSearchProvider).toBe('exa_free_mcp');

    act(() => result.current.discardSettingsChanges());
    expect(result.current.dirtyDomains).toEqual([]);
    expect(result.current.settingsConfig?.chat.model).toBe('next-model');
    expect(result.current.webSearchAction).toBe('keep');
  });

  it('删除 Exa 密钥时回退免费方案，重新选择 API 会取消待删除动作', () => {
    const { result } = renderHook(() => useSettingsDraft());
    act(() =>
      result.current.applyLoadedSettings({
        config,
        catalog,
        permissionMode: 'request_approval',
        sandboxMode: 'workspace_write',
        webSearchProvider: 'exa_api',
        webSearchConfigured: true,
        panel: 'web_search'
      })
    );

    act(() => result.current.stageWebSearchDelete());
    expect(result.current.webSearchProvider).toBe('exa_free_mcp');
    expect(result.current.webSearchAction).toBe('delete');

    act(() => result.current.stageWebSearchProvider('exa_api'));
    expect(result.current.webSearchProvider).toBe('exa_api');
    expect(result.current.webSearchAction).toBe('keep');
  });
});
