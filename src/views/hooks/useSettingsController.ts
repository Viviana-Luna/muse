import type { FormEvent } from 'react';

import {
  fetchDiagnosticsConnectivity,
  fetchAppearancePreferences,
  fetchModelCatalog,
  fetchModelInfo,
  fetchModelsConfig,
  fetchRuntimeWorkspaces,
  fetchWebSearchConfig,
  saveModelsConfig,
  saveAppearancePreferences,
  saveWebSearchConfig,
  updateRuntimeWorkspacePolicy
} from '@/api';
import { formatApiErrorMessage } from '@/api/client';
import type { AppToastInput } from '@/hooks/useAppToast';
import {
  formatModelInfoLabel,
  type ModelCatalog,
  type ModelConfigSection,
  type ModelsConfig
} from '@/types';
import type { SettingsPanel } from '@/views/settings/types';
import { DEFAULT_APPEARANCE_SETTINGS, type AppearanceSettings } from '@/views/settings/types';
import {
  normalizeSettingsConfig,
  useProviderCatalogController
} from './useProviderCatalogController';
import type { UseSettingsDraftResult } from './useSettingsDraft';

type ModelConfigPurpose = 'chat' | 'audio_understanding';

interface UseSettingsControllerOptions {
  draft: UseSettingsDraftResult;
  notify: (input: AppToastInput) => void;
  setBusy: (busy: boolean) => void;
  speak: (
    text: string,
    options?: { forceEnabled?: boolean; waitUntilEnded?: boolean }
  ) => Promise<string | null | undefined>;
  refreshVoiceCapabilities: () => Promise<unknown>;
  setModelLabel: (label: string) => void;
  navigateToStory: () => void;
}

/** 设置领域控制器：负责读取事实状态、分域保存与外部能力编排。 */
export function useSettingsController(options: UseSettingsControllerOptions) {
  const { draft } = options;
  const providers = useProviderCatalogController({
    draft,
    notify: options.notify,
    setBusy: options.setBusy,
    setModelLabel: options.setModelLabel
  });

  function appearanceFromResponse(response: {
    appearance: {
      background_blur: number;
      background_opacity: number;
      motion_level: AppearanceSettings['motionLevel'];
    };
  }): AppearanceSettings {
    return {
      backgroundBlur: response.appearance.background_blur,
      backgroundOpacity: response.appearance.background_opacity,
      motionLevel: response.appearance.motion_level
    };
  }

  function appearanceUpdate(settings: AppearanceSettings) {
    return {
      background_blur: settings.backgroundBlur,
      background_opacity: settings.backgroundOpacity,
      motion_level: settings.motionLevel
    };
  }

  function isDefaultAppearance(settings: AppearanceSettings) {
    return JSON.stringify(settings) === JSON.stringify(DEFAULT_APPEARANCE_SETTINGS);
  }

  function reportPreferenceDiagnostics(response: {
    diagnostics: Array<{ field_path: string; message: string }>;
  }) {
    if (response.diagnostics.length === 0) return;
    options.notify({
      title: '用户配置包含兼容诊断',
      description: response.diagnostics
        .slice(0, 3)
        .map((item) => `${item.field_path}：${item.message}`)
        .join('；'),
      tone: 'warning',
      duration: 12000
    });
  }

  /** 启动时从 config.toml 恢复外观，并一次性接管旧 localStorage 偏好。 */
  async function initializeAppearancePreferences() {
    try {
      let response = await fetchAppearancePreferences();
      const remote = appearanceFromResponse(response);
      const legacy = draft.legacyAppearanceSettings;
      if (
        legacy &&
        response.diagnostics.length === 0 &&
        isDefaultAppearance(remote) &&
        !isDefaultAppearance(legacy)
      ) {
        response = await saveAppearancePreferences(appearanceUpdate(legacy));
      }
      draft.markAppearanceSaved(appearanceFromResponse(response));
      reportPreferenceDiagnostics(response);
    } catch (error) {
      options.notify({
        title: '外观偏好读取失败',
        description: formatApiErrorMessage(error, '暂时使用内置外观，旧偏好尚未清理。'),
        tone: 'warning'
      });
    }
  }

  async function openSettings(panel: SettingsPanel = draft.settingsPanel) {
    options.setBusy(true);
    try {
      const [configResult, catalogResult, workspacesResult, webSearchResult] = await Promise.allSettled([
        fetchModelsConfig(),
        fetchModelCatalog(),
        fetchRuntimeWorkspaces(),
        fetchWebSearchConfig()
      ]);
      if (configResult.status === 'rejected') throw configResult.reason;
      const catalog: ModelCatalog =
        catalogResult.status === 'fulfilled'
          ? catalogResult.value
          : { providers: [], models: [], capabilities: [] };
      const workspaces =
        workspacesResult.status === 'fulfilled'
          ? workspacesResult.value
          : {
              permission_mode: draft.workspacePermissionMode,
              sandbox_mode: draft.workspaceSandboxMode
            };
      const webSearch =
        webSearchResult.status === 'fulfilled'
          ? webSearchResult.value
          : {
              provider: draft.webSearchProvider,
              api_key_configured: draft.webSearchConfigured
            };
      draft.applyLoadedSettings({
        config: normalizeSettingsConfig(configResult.value, catalog),
        catalog,
        permissionMode: workspaces.permission_mode,
        sandboxMode: workspaces.sandbox_mode,
        webSearchProvider: webSearch.provider,
        webSearchConfigured: webSearch.api_key_configured,
        panel
      });
      const failedDomains = [
        catalogResult.status === 'rejected' ? 'Provider 目录' : '',
        workspacesResult.status === 'rejected' ? '工具权限' : '',
        webSearchResult.status === 'rejected' ? '联网搜索' : ''
      ].filter(Boolean);
      if (failedDomains.length > 0) {
        options.notify({
          title: '部分设置暂时无法读取',
          description: `${failedDomains.join('、')}可在对应页面稍后重试。`,
          tone: 'warning'
        });
      }
    } catch (err) {
      options.notify({
        title: '设置读取失败',
        description: err instanceof Error ? err.message : '无法读取当前模型配置。',
        tone: 'error'
      });
      options.navigateToStory();
    } finally {
      options.setBusy(false);
    }
  }

  async function handlePreviewTts() {
    options.setBusy(true);
    try {
      const message = await options.speak('你好，我是你的虚拟助手。', {
        forceEnabled: true,
        waitUntilEnded: true
      });
      if (message) throw new Error(message);
      options.notify({ title: '测试播报完成', tone: 'success' });
    } catch (err) {
      const message = formatApiErrorMessage(err, '测试播报失败。');
      options.notify({ title: '测试播报失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function handleRefreshDiagnostics() {
    options.setBusy(true);
    try {
      const response = await fetchDiagnosticsConnectivity();
      draft.setDiagnosticsChecks(response.checks);
      options.notify({
        title: '连通性检测完成',
        description: `已完成 ${response.checks.length} 项检测。`,
        tone: 'success'
      });
    } catch (err) {
      const message = formatApiErrorMessage(err, '连通性检测失败。');
      options.notify({ title: '连通性检测失败', description: message, tone: 'error' });
    } finally {
      options.setBusy(false);
    }
  }

  function secretUpdateFor(
    section: ModelConfigPurpose | 'tts' | 'asr',
    currentKey: string | null | undefined
  ) {
    const current = currentKey ?? '';
    const masked = draft.maskedKeys[section];
    // 输入框对已保存密钥保持空值；空值的语义是“留空保持”，不能因为
    // 用户保存其他设置就误删凭据。删除密钥需要后续提供显式删除操作。
    if (!current) return { action: 'keep' } as const;
    if (
      current === masked ||
      current.includes('****') ||
      current.includes('••••')
    ) {
      return { action: 'keep' } as const;
    }
    return { action: 'replace', value: current } as const;
  }

  function buildSection(
    section: ModelConfigPurpose,
    original: ModelConfigSection
  ): ModelConfigSection {
    return {
      ...original,
      api_key: null,
      api_key_update: secretUpdateFor(section, original.api_key)
    };
  }

  async function submitSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const settingsConfig = draft.settingsConfig;
    if (!settingsConfig || draft.dirtyDomains.length === 0) return;
    options.setBusy(true);
    const failures: string[] = [];
    let modelsSaved = false;
    try {
      if (draft.dirtyDomains.includes('models')) {
        try {
          // 对话活动模型与供应商凭据由独立接口管理。保存语音等设置前重新读取
          // 最新聊天事实，避免一个较早打开的设置页把聊天页刚切换的模型覆盖回去。
          const latestRuntimeConfig = await fetchModelsConfig();
          const payload: ModelsConfig = {
            chat: buildSection('chat', latestRuntimeConfig.chat),
            tts: {
              ...settingsConfig.tts,
              api_key: null,
              api_key_update: secretUpdateFor('tts', settingsConfig.tts.api_key)
            },
            speech_recognition: {
              ...settingsConfig.speech_recognition,
              api_key: null,
              api_key_update: secretUpdateFor(
                'asr',
                settingsConfig.speech_recognition.api_key
              )
            },
            audio_understanding: buildSection(
              'audio_understanding',
              settingsConfig.audio_understanding
            ),
            voice_input: settingsConfig.voice_input
          };
          const saved = await saveModelsConfig(payload);
          const normalizedSaved: ModelsConfig = {
            ...saved,
            chat: {
              ...saved.chat,
              api_key: null
            },
            tts: {
              ...saved.tts,
              api_key: null
            },
            speech_recognition: {
              ...saved.speech_recognition,
              api_key: null
            },
            audio_understanding: {
              ...saved.audio_understanding,
              api_key: null
            }
          };
          draft.setMaskedKeys({
            chat: saved.chat.api_key_configured ? '••••••••' : '',
            tts: saved.tts.api_key_configured ? '••••••••' : '',
            asr: saved.speech_recognition.api_key_configured ? '••••••••' : '',
            audio_understanding: saved.audio_understanding.api_key_configured
              ? '••••••••'
              : ''
          });
          draft.markModelsSaved(normalizedSaved);
          modelsSaved = true;
        } catch (err) {
          failures.push(`模型：${formatApiErrorMessage(err, '保存失败')}`);
        }
      }

      if (draft.dirtyDomains.includes('workspace')) {
        try {
          const response = await updateRuntimeWorkspacePolicy(
            draft.workspacePermissionMode,
            draft.workspaceSandboxMode
          );
          draft.markWorkspaceSaved(response.permission_mode, response.sandbox_mode);
        } catch (err) {
          const message = formatApiErrorMessage(err, '保存失败');
          failures.push(`权限：${message}`);
        }
      }

      if (draft.dirtyDomains.includes('web_search')) {
        try {
          const update =
            draft.webSearchAction === 'replace'
              ? ({
                  provider: draft.webSearchProvider,
                  action: 'replace',
                  value: draft.webSearchKeyDraft.trim()
                } as const)
              : draft.webSearchAction === 'delete'
                ? ({ provider: draft.webSearchProvider, action: 'delete' } as const)
                : ({ provider: draft.webSearchProvider, action: 'keep' } as const);
          if (update.action === 'replace' && !update.value) {
            throw new Error('请输入有效的 Exa API Key。');
          }
          if (
            update.provider === 'exa_api' &&
            update.action !== 'replace' &&
            !draft.webSearchConfigured
          ) {
            throw new Error('Exa API Key 模式需要先配置有效密钥。');
          }
          const webSearch = await saveWebSearchConfig(update);
          draft.applyWebSearchState(webSearch.provider, webSearch.api_key_configured);
        } catch (err) {
          const message = formatApiErrorMessage(err, '保存失败');
          failures.push(`联网搜索：${message}`);
        }
      }

      if (draft.dirtyDomains.includes('appearance')) {
        try {
          const saved = await saveAppearancePreferences(
            appearanceUpdate(draft.appearanceSettings)
          );
          draft.markAppearanceSaved(appearanceFromResponse(saved));
          reportPreferenceDiagnostics(saved);
        } catch (err) {
          failures.push(`外观：${formatApiErrorMessage(err, '保存失败')}`);
        }
      }

      if (modelsSaved) {
        try {
          const model = await fetchModelInfo();
          options.setModelLabel(formatModelInfoLabel(model));
        } catch {
          // 展示标签刷新失败不回滚已成功提交的设置。
        }
        await Promise.allSettled([options.refreshVoiceCapabilities()]);
      }

      if (failures.length > 0) {
        options.notify({
          title: '部分设置保存失败',
          description: `${failures.join('；')}。失败领域的草稿已保留。`,
          tone: 'error',
          duration: 12000
        });
        return;
      }

      options.navigateToStory();
      options.notify({ title: '全部更改已保存', description: '新配置已应用。', tone: 'success' });
    } finally {
      options.setBusy(false);
    }
  }

  return {
    initializeAppearancePreferences,
    openSettings,
    submitSettings,
    handleDeleteWebSearchKey: draft.stageWebSearchDelete,
    handleUpdateWorkspacePolicy: draft.stageWorkspacePolicy,
    handlePreviewTts,
    handleRefreshDiagnostics,
    ...providers
  };
}
