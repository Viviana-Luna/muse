import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ComponentProps } from 'react';

import { ChatPanel } from './ChatPanel';
import type { ModelCatalog, ModelProviderCatalog, ModelsConfig } from '@/types';

afterEach(cleanup);

const deepSeekProvider: ModelProviderCatalog = {
  id: 'deepseek',
  name: 'DeepSeek',
  default_api_base: 'https://api.deepseek.com',
  chat_model_list_url: '/models',
  tts_model_list_url: '',
  enabled: true,
  notes: '',
  supports_balance_check: true,
  capabilities: ['chat'],
  model_list_auth: 'required',
  allow_custom_base: false,
  connection_validation: 'model_list',
  status: 'supported',
  api_key_configured: true
};

const catalog: ModelCatalog = {
  providers: [deepSeekProvider],
  models: [
    {
      id: 'deepseek:deepseek-v4-flash',
      provider_id: 'deepseek',
      name: 'DeepSeek V4 Flash',
      model: 'deepseek-v4-flash',
      default_api_base: deepSeekProvider.default_api_base,
      enabled: true,
      notes: '',
      tags: ['reasoning', 'tool'],
      functions: ['chat'],
      capabilities: ['chat', 'reasoning', 'tool'],
      context_window: 128_000,
      default_max_output_tokens: 2_048,
      supports_usage: true,
      supports_cached_tokens: false,
      supports_reasoning_tokens: true,
      tokenizer_family: 'rough_estimate'
    }
  ],
  capabilities: ['chat', 'reasoning', 'tool']
};

function createSettingsConfig(): ModelsConfig {
  return {
    chat: {
      provider: 'deepseek',
      api_base: deepSeekProvider.default_api_base,
      api_key: null,
      api_key_configured: true,
      model: 'deepseek-v4-flash'
    },
    tts: {
      enabled: false,
      provider: '',
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
      provider: '',
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
}

function createProps(): ComponentProps<typeof ChatPanel> {
  return {
    settingsConfig: createSettingsConfig(),
    modelCatalog: catalog,
    busy: false,
    onSaveProviderCredential: vi.fn(),
    onDeleteProviderCredential: vi.fn(),
    onSetProviderEnabled: vi.fn(),
    onVerifyCatalogProvider: vi.fn(),
    onCreateCatalogModel: vi.fn(),
    onUpdateCatalogModel: vi.fn(),
    onDeleteCatalogModel: vi.fn(),
    capabilityBadges: () => <span>对话 推理 工具</span>
  };
}

describe('ChatPanel', () => {
  it('按供应商主从布局展示连接与模型，并标记当前活动模型', () => {
    const { container } = render(<ChatPanel {...createProps()} />);

    expect(container.querySelector('.provider-settings-workspace')).toBeInTheDocument();
    expect(container.querySelector('.provider-directory-column')).toBeInTheDocument();
    expect(container.querySelector('.provider-detail-column')).toBeInTheDocument();
    expect(container.querySelector('.provider-detail-scroll')).toBeInTheDocument();
    expect(container.querySelector('.provider-orb')).not.toBeInTheDocument();
    expect(container.querySelector('.provider-library-list svg')).not.toBeInTheDocument();
    expect(screen.getByText('模型提供商')).toBeVisible();
    expect(screen.getByRole('heading', { name: 'API 密钥' })).toBeVisible();
    expect(screen.getByRole('heading', { name: 'API 地址' })).toBeVisible();
    expect(screen.queryByRole('searchbox')).not.toBeInTheDocument();
    expect(screen.getByLabelText('API Base')).toHaveValue('https://api.deepseek.com');
    expect(screen.getByLabelText('API Base')).toHaveAttribute('readonly');
    expect(screen.getAllByText('DeepSeek').length).toBeGreaterThan(0);
    expect(screen.getByText('DeepSeek V4 Flash')).toBeVisible();
    expect(screen.getByText('当前对话')).toBeVisible();
  });

  it('已保存的供应商密钥不回显内容，并允许独立验证连接', () => {
    const props = createProps();
    render(<ChatPanel {...props} />);

    expect(screen.getByLabelText('API Key')).toHaveValue('');
    expect(screen.getByRole('button', { name: '验证连接' })).toBeEnabled();
    fireEvent.click(screen.getByRole('button', { name: '验证连接' }));
    expect(props.onVerifyCatalogProvider).toHaveBeenCalledWith(
      'deepseek',
      'deepseek-v4-flash'
    );
  });

  it('新增模型只能从现有供应商选择，编辑表单不会承载 API Key', () => {
    render(<ChatPanel {...createProps()} />);
    fireEvent.click(screen.getByRole('button', { name: '新增模型' }));

    expect(screen.getByRole('dialog', { name: '新增模型' })).toBeVisible();
    expect(screen.getByRole('combobox')).toHaveValue('deepseek');
    expect(
      screen.getByRole('dialog', { name: '新增模型' }).querySelector('input[type="password"]')
    ).toBeNull();
    expect(screen.getByText(/API Key 不保存在模型上/)).toBeVisible();
  });

  it('关闭的供应商仍可管理，并通过可访问开关请求开启', () => {
    const props = createProps();
    props.modelCatalog = {
      ...catalog,
      providers: [{ ...deepSeekProvider, enabled: false }]
    };
    render(<ChatPanel {...props} />);

    expect(screen.getAllByText('已关闭').length).toBeGreaterThan(0);
    const toggle = screen.getByRole('switch', { name: '开启 DeepSeek' });
    expect(toggle).toHaveAttribute('aria-checked', 'false');
    fireEvent.click(toggle);
    expect(props.onSetProviderEnabled).toHaveBeenCalledWith('deepseek', true);
  });
});
