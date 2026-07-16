import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  fetchModelCatalog,
  fetchModelsConfig,
  saveActiveChatModel,
  saveProviderCredential
} from '@/api';
import type { ModelCatalog, ModelCatalogItem, ModelsConfig } from '@/types';
import { useProviderCatalogController } from './useProviderCatalogController';
import { useSettingsDraft } from './useSettingsDraft';

vi.mock('@/api', () => ({
  fetchModelCatalog: vi.fn(),
  fetchModelsConfig: vi.fn(),
  fetchProviderBalance: vi.fn(),
  fetchProviderModels: vi.fn(),
  createCatalogModel: vi.fn(),
  updateCatalogModel: vi.fn(),
  deleteCatalogModel: vi.fn(),
  saveProviderCredential: vi.fn(),
  saveActiveChatModel: vi.fn()
}));

const agentPlanModel: ModelCatalogItem = {
  id: 'volcengine_agent_plan:doubao-seed-2.0-pro',
  provider_id: 'volcengine_agent_plan',
  name: 'Doubao Seed 2.0 Pro',
  model: 'doubao-seed-2.0-pro',
  default_api_base: 'https://ark.cn-beijing.volces.com/api/plan/v3',
  enabled: true,
  notes: 'Agent Plan 套餐模型',
  tags: ['reasoning', 'tool'],
  functions: ['chat'],
  capabilities: ['chat', 'reasoning', 'tool'],
  context_window: 256_000,
  default_max_output_tokens: 2048,
  supports_usage: true,
  supports_cached_tokens: false,
  supports_reasoning_tokens: true,
  tokenizer_family: 'ark'
};

const catalog: ModelCatalog = {
  providers: [
    {
      id: 'volcengine_agent_plan',
      name: '火山方舟 Agent Plan',
      default_api_base: agentPlanModel.default_api_base,
      chat_model_list_url: '',
      tts_model_list_url: '',
      enabled: true,
      notes: '',
      supports_balance_check: false,
      capabilities: ['chat'],
      model_list_auth: 'required',
      allow_custom_base: false,
      connection_validation: 'chat_probe',
      status: 'supported',
      api_key_configured: true
    }
  ],
  models: [agentPlanModel],
  capabilities: ['chat', 'reasoning', 'tool']
};

const config: ModelsConfig = {
  chat: {
    provider: 'volcengine_agent_plan',
    api_base: agentPlanModel.default_api_base,
    api_key: null,
    api_key_configured: true,
    model: 'glm-5.2',
    max_tokens: 1_000_000,
    temperature: 0.7
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
    enabled: false,
    provider: 'openai_audio_transcriptions',
    api_base: '',
    api_key: null,
    api_key_configured: false,
    model: '',
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

describe('useProviderCatalogController', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(fetchModelsConfig).mockResolvedValue(config);
    vi.mocked(fetchModelCatalog).mockResolvedValue(catalog);
    vi.mocked(saveActiveChatModel).mockResolvedValue({
      provider_id: 'volcengine_agent_plan',
      provider_name: '火山方舟 Agent Plan',
      model: 'doubao-seed-2.0-pro',
      model_name: 'Doubao Seed 2.0 Pro'
    });
  });

  it('聊天页模型选择器通过活动模型接口直接切换双要素模型', async () => {
    const notify = vi.fn();
    const setBusy = vi.fn();
    const setModelLabel = vi.fn();
    const { result } = renderHook(() => {
      const draft = useSettingsDraft();
      const controller = useProviderCatalogController({
        draft,
        notify,
        setBusy,
        setModelLabel
      });
      return { draft, controller };
    });

    await act(async () => result.current.controller.openRuntimeModelPicker());
    expect(result.current.draft.modelPicker).toMatchObject({
      purpose: 'chat',
      models: [agentPlanModel],
      currentProvider: 'volcengine_agent_plan',
      currentModel: 'glm-5.2',
      commitOnSelect: true
    });

    await act(async () =>
      result.current.controller.chooseFetchedModel('chat', agentPlanModel)
    );

    expect(saveActiveChatModel).toHaveBeenCalledWith(
      'volcengine_agent_plan',
      'doubao-seed-2.0-pro'
    );
    expect(setModelLabel).toHaveBeenCalledWith(
      '火山方舟 Agent Plan / Doubao Seed 2.0 Pro'
    );
    expect(result.current.draft.modelPicker).toBeNull();
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({ title: '聊天模型已切换', tone: 'success' })
    );
  });

  it('未配置或旧供应商状态也能在首次发送前打开模型选择器', async () => {
    vi.mocked(fetchModelsConfig).mockResolvedValue({
      ...config,
      chat: { ...config.chat, provider: '', model: '' }
    });
    const { result } = renderHook(() => {
      const draft = useSettingsDraft();
      return {
        draft,
        controller: useProviderCatalogController({
          draft,
          notify: vi.fn(),
          setBusy: vi.fn(),
          setModelLabel: vi.fn()
        })
      };
    });

    await act(async () => result.current.controller.openRuntimeModelPicker());

    expect(result.current.draft.modelPicker).toMatchObject({
      purpose: 'chat',
      models: [agentPlanModel],
      currentProvider: '',
      currentModel: '',
      commitOnSelect: true
    });
  });

  it('凭据写入成功但目录刷新失败时不再误报写入失败', async () => {
    vi.mocked(saveProviderCredential).mockResolvedValue({
      provider_id: 'volcengine_agent_plan',
      api_key_configured: true,
      status: '供应商凭据已保存。'
    });
    vi.mocked(fetchModelCatalog).mockRejectedValueOnce(new Error('目录暂时不可用'));
    const notify = vi.fn();
    const { result } = renderHook(() => {
      const draft = useSettingsDraft();
      return {
        draft,
        controller: useProviderCatalogController({
          draft,
          notify,
          setBusy: vi.fn(),
          setModelLabel: vi.fn()
        })
      };
    });
    act(() => result.current.draft.setModelCatalog(catalog));

    await act(async () =>
      result.current.controller.saveCatalogProviderCredential(
        'volcengine_agent_plan',
        'test-only-key'
      )
    );

    expect(result.current.draft.modelCatalog?.providers[0]?.api_key_configured).toBe(true);
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({
        title: '供应商凭据已保存，目录暂未刷新',
        tone: 'warning'
      })
    );
    expect(notify).not.toHaveBeenCalledWith(
      expect.objectContaining({ title: '保存供应商凭据失败' })
    );
  });
});
