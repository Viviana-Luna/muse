import type { AppToastInput } from '@/hooks/useAppToast';
import type {
  ModelCatalog,
  ModelCatalogItem,
  ModelCatalogMutation,
  ModelConfigSection,
  ModelProviderCatalog,
  ModelsConfig,
  ProviderBalanceResponse
} from '@/types';
import {
  createCatalogModel,
  deleteCatalogModel,
  fetchModelCatalog,
  fetchModelsConfig,
  fetchProviderBalance,
  fetchProviderModels,
  saveActiveChatModel,
  saveProviderEnabled,
  saveProviderCredential,
  updateCatalogModel
} from '@/api';
import { formatApiErrorMessage } from '@/api/client';
import type { ModelPurpose } from '@/views/settings/types';
import type { UseSettingsDraftResult } from './useSettingsDraft';

const PURPOSE_CAPABILITIES: Record<ModelPurpose, string[]> = {
  chat: ['chat']
};

const CAPABILITY_LABELS: Record<string, string> = {
  chat: '对话',
  tool: '工具',
  image_understanding: '图像识别',
  image_generation: '图像生成',
  tts: '语音合成',
  audio_understanding: '音频理解',
  reasoning: '推理'
};

export function modelSupportsPurpose(model: ModelCatalogItem, purpose: ModelPurpose): boolean {
  if (!model.enabled) return false;
  return PURPOSE_CAPABILITIES[purpose].every((capability) =>
    model.functions.includes(capability)
  );
}

export function normalizeSettingsConfig(config: ModelsConfig, _catalog: ModelCatalog): ModelsConfig {
  const normalizeSection = (section: ModelConfigSection): ModelConfigSection => {
    // 后端只返回 api_key_configured；不能把掩码写进 input.value，
    // 否则用户点击“显示”时只能看到另一串掩码，而无法输入新的密钥。
    return { ...section, api_key: null, api_protocol: 'chat_completions' };
  };

  return {
    ...config,
    chat: normalizeSection(config.chat),
    tts: {
      ...config.tts,
      api_key: null
    },
    speech_recognition: {
      ...config.speech_recognition,
      api_key: null
    },
    audio_understanding: {
      ...config.audio_understanding,
      api_key: null
    }
  };
}

interface UseProviderCatalogControllerOptions {
  draft: UseSettingsDraftResult;
  notify: (input: AppToastInput) => void;
  setBusy: (busy: boolean) => void;
  setModelLabel: (label: string) => void;
}

export function useProviderCatalogController(options: UseProviderCatalogControllerOptions) {
  const { draft } = options;

  function modelsForPurpose(purpose: ModelPurpose, providerId: string): ModelCatalogItem[] {
    if (!draft.modelCatalog) return [];
    const providerEnabled = draft.modelCatalog.providers.some(
      (provider) => provider.id === providerId && provider.enabled
    );
    if (!providerEnabled) return [];
    return draft.modelCatalog.models.filter(
      (model) =>
        model.provider_id === providerId && modelSupportsPurpose(model, purpose)
    );
  }

  function selectedCatalogModel(
    purpose: ModelPurpose,
    section: ModelConfigSection
  ): ModelCatalogItem | null {
    return (
      modelsForPurpose(purpose, section.provider).find(
        (model) => model.model === section.model
      ) ?? null
    );
  }

  function capabilityBadges(model?: ModelCatalogItem | null) {
    if (!model) {
      return (
        <span className="field-hint">
          当前模型不在内置能力目录中，能力以手动配置为准。
        </span>
      );
    }
    if (model.capabilities.length === 0) {
      return (
        <span className="capability-row" aria-label="模型能力">
          <span className="capability-pill unknown">能力未标注</span>
        </span>
      );
    }
    return (
      <span className="capability-row" aria-label="模型能力">
        {model.capabilities.map((capability) => (
          <span className={`capability-pill ${capability}`} key={capability}>
            {CAPABILITY_LABELS[capability] ?? capability}
          </span>
        ))}
      </span>
    );
  }

  function providersForPurpose(purpose: ModelPurpose) {
    return (draft.modelCatalog?.providers ?? []).filter(
      (provider) => provider.enabled && provider.capabilities.includes(purpose)
    );
  }

  function defaultApiBaseForProvider(providerId: string): string {
    const catalogBase =
      draft.modelCatalog?.providers.find((provider) => provider.id === providerId)
        ?.default_api_base ?? '';
    return catalogBase;
  }

  function providerSupportsBalance(providerId: string): boolean {
    const provider = draft.modelCatalog?.providers.find((item) => item.id === providerId);
    return !!provider?.supports_balance_check;
  }

  function providerAllowsCustomBase(providerId: string): boolean {
    return draft.modelCatalog?.providers.find((provider) => provider.id === providerId)
      ?.allow_custom_base ?? false;
  }

  function updateProvider(section: ModelPurpose, providerId: string) {
    const models = modelsForPurpose(section, providerId);
    const firstModel = models.find((model) => model.model.trim()) ?? models[0];
    draft.setSettingsConfig((state) => {
      if (!state) return state;
      return {
        ...state,
        [section]: {
          ...state[section],
          provider: providerId,
          api_base: firstModel?.default_api_base || defaultApiBaseForProvider(providerId),
          model: firstModel?.model ?? '',
          max_tokens: firstModel?.default_max_output_tokens ?? state[section].max_tokens
        }
      };
    });
    draft.setModelFetchStatus((state) => ({
      ...state,
      [section]: '已切换提供商，请验证连接。'
    }));
  }

  function apiKeyForFetch(section: ModelPurpose): string | null {
    if (!draft.settingsConfig) return null;
    const current = draft.settingsConfig[section].api_key ?? '';
    if (
      !current ||
      current === draft.maskedKeys[section] ||
      current.includes('****') ||
      current.includes('••••')
    ) {
      return null;
    }
    return current;
  }

  function formatProviderBalance(response: ProviderBalanceResponse): string {
    const balances = response.balance_infos
      .map(
        (item) =>
          `${item.currency} ${item.total_balance}（赠额 ${item.granted_balance}，充值 ${item.topped_up_balance}）`
      )
      .join('；');
    const prefix = response.is_available ? '余额可用' : '余额不可用';
    return balances ? `${prefix}：${balances}` : response.status;
  }

  async function checkProviderBalance(section: ModelPurpose) {
    if (!draft.settingsConfig) return;
    const config = draft.settingsConfig[section];
    if (!config.provider) {
      options.notify({ title: '请先选择提供商', tone: 'warning' });
      return;
    }
    if (!providerSupportsBalance(config.provider)) {
      options.notify({ title: '暂不支持余额检测', tone: 'warning' });
      return;
    }
    if (!config.api_base) {
      options.notify({ title: '请先配置 API Base', tone: 'warning' });
      return;
    }

    options.setBusy(true);
    try {
      const response = await fetchProviderBalance({
        provider: config.provider,
        purpose: section,
        api_base: config.api_base,
        api_key: apiKeyForFetch(section)
      });
      const status = formatProviderBalance(response);
      options.notify({
        title: response.is_available ? '供应商余额可用' : '供应商余额不可用',
        description: status,
        tone: response.is_available ? 'success' : 'warning'
      });
    } catch (err) {
      const message = formatApiErrorMessage(err, '余额检测失败');
      options.notify({ title: '余额检测失败', description: message, tone: 'error' });
    } finally {
      options.setBusy(false);
    }
  }

  async function loadProviderModels(section: ModelPurpose) {
    if (!draft.settingsConfig) return;
    const config = draft.settingsConfig[section];
    if (!config.provider) {
      draft.setModelFetchStatus((state) => ({ ...state, [section]: '请先选择提供商。' }));
      options.notify({ title: '请先选择提供商', tone: 'warning' });
      return;
    }
    if (!config.api_base) {
      draft.setModelFetchStatus((state) => ({ ...state, [section]: '请先配置 API Base。' }));
      options.notify({ title: '请先配置 API Base', tone: 'warning' });
      return;
    }
    options.setBusy(true);
    draft.setModelFetchStatus((state) => ({ ...state, [section]: '正在获取模型列表...' }));
    try {
      const response = await fetchProviderModels({
        provider: config.provider,
        purpose: section,
        api_base: config.api_base,
        model: config.model,
        api_key: apiKeyForFetch(section)
      });
      draft.setModelCatalog((catalog) => {
        if (!catalog) return catalog;
        const existing = catalog.models.filter(
          (model) =>
            model.provider_id !== response.provider_id ||
            !modelSupportsPurpose(model, section)
        );
        return { ...catalog, models: [...existing, ...response.models] };
      });
      draft.setModelPicker({
        purpose: section,
        models: response.models,
        currentModel: config.model
      });
      draft.setModelFetchStatus((state) => ({
        ...state,
        [section]: `已获取 ${response.models.length} 个模型。`
      }));
      options.notify({
        title: '模型列表已加载',
        description: `共获取 ${response.models.length} 个可选模型。`,
        tone: 'success'
      });
    } catch (err) {
      const message = formatApiErrorMessage(err, '获取模型列表失败');
      draft.setModelFetchStatus((state) => ({ ...state, [section]: message }));
      options.notify({ title: '获取模型列表失败', description: message, tone: 'error' });
    } finally {
      options.setBusy(false);
    }
  }

  async function refreshCatalog() {
    const catalog = await fetchModelCatalog();
    draft.setModelCatalog(catalog);
    return catalog;
  }

  async function finishCommittedCatalogMutation(title: string, description?: string) {
    try {
      await refreshCatalog();
      options.notify({ title, description, tone: 'success' });
    } catch (err) {
      options.notify({
        title: `${title}，目录暂未刷新`,
        description: `${description ? `${description}；` : ''}${formatApiErrorMessage(
          err,
          '重新读取模型目录失败'
        )}`,
        tone: 'warning',
        duration: 10000
      });
    }
  }

  function setProviderCredentialState(providerId: string, configured: boolean) {
    draft.setModelCatalog((current) => current && ({
      ...current,
      providers: current.providers.map((provider) =>
        provider.id === providerId
          ? { ...provider, api_key_configured: configured }
          : provider
      )
    }));
  }

  function upsertManagedModel(model: ModelCatalogItem) {
    draft.setModelCatalog((current) => current && ({
      ...current,
      models: [
        ...current.models.filter(
          (item) => item.provider_id !== model.provider_id || item.model !== model.model
        ),
        model
      ]
    }));
  }

  async function saveCatalogProviderCredential(providerId: string, secret: string) {
    const value = secret.trim();
    if (!value) {
      options.notify({ title: '请输入 API Key', tone: 'warning' });
      return;
    }
    options.setBusy(true);
    try {
      const response = await saveProviderCredential(providerId, {
        action: 'replace',
        value
      });
      setProviderCredentialState(providerId, response.api_key_configured);
      await finishCommittedCatalogMutation('供应商凭据已保存', response.status);
    } catch (err) {
      const message = formatApiErrorMessage(err, '保存供应商凭据失败');
      options.notify({ title: '保存供应商凭据失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function deleteCatalogProviderCredential(providerId: string) {
    options.setBusy(true);
    try {
      const response = await saveProviderCredential(providerId, { action: 'delete' });
      setProviderCredentialState(providerId, response.api_key_configured);
      await finishCommittedCatalogMutation('供应商凭据已删除', response.status);
    } catch (err) {
      const message = formatApiErrorMessage(err, '删除供应商凭据失败');
      options.notify({ title: '删除供应商凭据失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function setCatalogProviderEnabled(providerId: string, enabled: boolean) {
    options.setBusy(true);
    try {
      const disablingActiveProvider =
        !enabled && draft.settingsConfig?.chat.provider === providerId;
      const provider = await saveProviderEnabled(providerId, enabled);
      draft.setModelCatalog((current) => current && ({
        ...current,
        providers: current.providers.map((item) =>
          item.id === provider.id ? provider : item
        )
      }));
      if (disablingActiveProvider && draft.settingsConfig) {
        const config = {
          ...draft.settingsConfig,
          chat: {
            ...draft.settingsConfig.chat,
            provider: '',
            api_base: '',
            model: '',
            api_key: null,
            api_key_configured: false
          }
        };
        draft.markModelsSaved(config);
        options.setModelLabel('未配置 / 未选择模型');
      }
      await finishCommittedCatalogMutation(
        enabled ? '供应商已开启' : '供应商已关闭',
        disablingActiveProvider
          ? `${provider.name} 已关闭，活动聊天模型已清空。`
          : enabled
          ? `${provider.name} 的模型现在可以用于对话。`
          : `${provider.name} 的模型已从运行时选择器隐藏。`
      );
    } catch (err) {
      const message = formatApiErrorMessage(err, enabled ? '开启供应商失败' : '关闭供应商失败');
      options.notify({
        title: enabled ? '开启供应商失败' : '关闭供应商失败',
        description: message,
        tone: 'error'
      });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function verifyCatalogProvider(providerId: string, model?: string) {
    const provider = draft.modelCatalog?.providers.find((item) => item.id === providerId);
    if (!provider) {
      options.notify({ title: '供应商目录已失效', description: '请重新打开设置页。', tone: 'error' });
      return;
    }
    if (!provider.api_key_configured) {
      options.notify({
        title: '请先保存供应商凭据',
        description: '未配置 API Key 时不会发送上游请求。',
        tone: 'warning'
      });
      return;
    }
    options.setBusy(true);
    try {
      const response = await fetchProviderModels({
        provider: provider.id,
        purpose: 'chat',
        api_base: provider.default_api_base,
        model: model || null,
        api_key: null
      });
      options.notify({
        title: '供应商连接正常',
        description: response.models.length > 0
          ? `验证成功，可识别 ${response.models.length} 个模型。`
          : '验证成功。',
        tone: 'success'
      });
    } catch (err) {
      const message = formatApiErrorMessage(err, '供应商连接验证失败');
      options.notify({ title: '供应商连接验证失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function createManagedModel(model: ModelCatalogMutation) {
    options.setBusy(true);
    try {
      const created = await createCatalogModel(model);
      upsertManagedModel(created);
      await finishCommittedCatalogMutation(
        '模型已创建',
        `${created.name} · ${created.model}`
      );
    } catch (err) {
      const message = formatApiErrorMessage(err, '创建模型失败');
      options.notify({ title: '创建模型失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function updateManagedModel(model: ModelCatalogMutation) {
    options.setBusy(true);
    try {
      const updated = await updateCatalogModel(model);
      upsertManagedModel(updated);
      await finishCommittedCatalogMutation(
        '模型已更新',
        `${updated.name} · ${updated.model}`
      );
    } catch (err) {
      const message = formatApiErrorMessage(err, '更新模型失败');
      options.notify({ title: '更新模型失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function deleteManagedModel(providerId: string, model: string) {
    options.setBusy(true);
    try {
      await deleteCatalogModel(providerId, model);
      draft.setModelCatalog((current) => current && ({
        ...current,
        models: current.models.filter(
          (item) => item.provider_id !== providerId || item.model !== model
        )
      }));
      await finishCommittedCatalogMutation('模型已删除');
    } catch (err) {
      const message = formatApiErrorMessage(err, '删除模型失败');
      options.notify({ title: '删除模型失败', description: message, tone: 'error' });
      throw err;
    } finally {
      options.setBusy(false);
    }
  }

  async function openRuntimeModelPicker() {
    options.setBusy(true);
    try {
      const [config, catalog] = await Promise.all([fetchModelsConfig(), fetchModelCatalog()]);
      const enabledProviderIds = new Set(
        catalog.providers.filter((provider) => provider.enabled).map((provider) => provider.id)
      );
      const models = catalog.models.filter(
        (model) =>
          enabledProviderIds.has(model.provider_id) && modelSupportsPurpose(model, 'chat')
      );
      if (models.length === 0) {
        throw new Error('没有已开启供应商的可选聊天模型，请先到设置中心完成配置并开启供应商。');
      }

      draft.setModelCatalog(catalog);
      draft.setModelFetchStatus((state) => ({
        ...state,
        chat: '选择后会立即应用到后续对话。'
      }));
      draft.setModelPicker({
        purpose: 'chat',
        models,
        currentProvider: config.chat.provider,
        currentModel: config.chat.model,
        commitOnSelect: true
      });
    } catch (err) {
      const message = formatApiErrorMessage(err, '读取模型目录失败');
      options.notify({ title: '无法打开模型选择器', description: message, tone: 'error' });
    } finally {
      options.setBusy(false);
    }
  }

  async function chooseFetchedModel(section: ModelPurpose, model: ModelCatalogItem) {
    if (draft.modelPicker?.commitOnSelect) {
      options.setBusy(true);
      try {
        const saved = await saveActiveChatModel(model.provider_id, model.model);
        options.setModelLabel(`${saved.provider_name} / ${saved.model_name}`);
        draft.setModelFetchStatus((state) => ({
          ...state,
          chat: `已切换到 ${saved.provider_name} / ${saved.model_name}。`
        }));
        draft.setModelPicker(null);
        options.notify({
          title: '聊天模型已切换',
          description: `${model.name} · ${model.model}`,
          tone: 'success'
        });
      } catch (err) {
        const message = formatApiErrorMessage(err, '切换模型失败');
        options.notify({ title: '切换模型失败', description: message, tone: 'error' });
      } finally {
        options.setBusy(false);
      }
      return;
    }

    draft.setSettingsConfig((state) => {
      if (!state) return state;
      return {
        ...state,
        [section]: {
          ...state[section],
          model: model.model,
          api_base: model.default_api_base || state[section].api_base,
          max_tokens: model.default_max_output_tokens
        }
      };
    });
    draft.setModelPicker(null);
  }

  return {
    modelsForPurpose,
    selectedCatalogModel,
    capabilityBadges,
    providersForPurpose,
    providerAllowsCustomBase,
    providerSupportsBalance,
    updateProvider,
    checkProviderBalance,
    loadProviderModels,
    saveCatalogProviderCredential,
    deleteCatalogProviderCredential,
    setCatalogProviderEnabled,
    verifyCatalogProvider,
    createManagedModel,
    updateManagedModel,
    deleteManagedModel,
    openRuntimeModelPicker,
    chooseFetchedModel
  };
}
