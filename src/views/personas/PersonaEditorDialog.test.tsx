import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApiError } from '@/api/client';
import type { ModelCatalog, Persona } from '@/types';
import type { PersonaEditorState } from '@/views/personas/hooks/usePersonaState';
import { PersonaEditorDialog } from './PersonaEditorDialog';

const api = vi.hoisted(() => ({ uploadPersonaImage: vi.fn() }));

vi.mock('@/api', () => api);

afterEach(() => {
  cleanup();
  document.querySelectorAll('.runtime-shell').forEach((element) => element.remove());
  vi.clearAllMocks();
});

const persona: Persona = {
  id: 'muse',
  name: 'Muse',
  summary: '',
  character_profile: '可靠',
  world_profile: '现实日常',
  scenario: '',
  system_prompt: '保持角色。',
  style: '',
  roleplay_style: 'light_narration',
  dialogue_examples: '',
  author_note: '',
  opening_message: '你好。',
  tool_policy: { mode: 'inherit', allowed_tools: [] },
  skill_policy: { mode: 'inherit', allowed_skills: [] },
  mcp_policy: { mode: 'inherit', allowed_servers: [] },
  preferred_model_ref: null,
  preferred_voice_id: null,
  default_visual_pack_id: 'default',
  author: '',
  version: '1.0.0',
  notes: ''
};

const modelCatalog: ModelCatalog = {
  providers: [
    {
      id: 'deepseek',
      name: 'DeepSeek',
      default_api_base: 'https://api.deepseek.com',
      chat_model_list_url: '',
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
    }
  ],
  models: [
    {
      id: 'deepseek:deepseek-v4-pro',
      provider_id: 'deepseek',
      name: 'DeepSeek V4 Pro',
      model: 'deepseek-v4-pro',
      default_api_base: 'https://api.deepseek.com',
      enabled: true,
      notes: '',
      tags: ['reasoning'],
      functions: ['chat'],
      capabilities: ['chat', 'reasoning'],
      context_window: 1_000_000,
      default_max_output_tokens: 384_000,
      supports_usage: true,
      supports_cached_tokens: true,
      supports_reasoning_tokens: true,
      tokenizer_family: 'deepseek'
    }
  ],
  capabilities: ['chat', 'reasoning']
};

function renderEditor(
  editorPatch: Partial<PersonaEditorState> = {},
  propsPatch: Partial<ComponentProps<typeof PersonaEditorDialog>> = {}
) {
  const editor: PersonaEditorState = {
    open: true,
    mode: 'create',
    persona,
    visualPackDraft: { portrait_path: '' },
    visualDirty: false,
    ...editorPatch
  };
  const notify = vi.fn();
  const onSave = vi.fn().mockResolvedValue(true);
  render(
    <PersonaEditorDialog
      editor={editor}
      busy={false}
      notify={notify}
      onClose={vi.fn()}
      onSave={onSave}
      setEditor={vi.fn()}
      updateEditor={vi.fn()}
      updateEditorVisualDraft={vi.fn()}
      updateEditorToolPolicyMode={vi.fn()}
      updateEditorAllowedTools={vi.fn()}
      {...propsPatch}
    />
  );
  return { notify, onSave };
}

describe('PersonaEditorDialog', () => {
  it('角色 ID 的 HTML pattern 可按 Unicode Sets 规则解析', () => {
    renderEditor();
    const idInput = screen.getByRole('textbox', { name: '角色 ID' });
    const pattern = idInput.getAttribute('pattern');

    expect(pattern).toBe('[A-Za-z0-9_\\-]+');
    expect(() => new RegExp(pattern!, 'v')).not.toThrow();
  });

  it('首次创建路径可以明确呈现创建并启用主操作', () => {
    renderEditor({}, { primaryActionLabel: '创建并启用' });
    expect(screen.getByRole('button', { name: '创建并启用' })).toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: '角色编辑' })).toHaveClass('stacked');
  });

  it('应用壳存在时挂载到根级 Portal，避免被功能栏层叠上下文遮挡', () => {
    const portalHost = document.createElement('main');
    portalHost.className = 'runtime-shell';
    document.body.appendChild(portalHost);

    renderEditor();

    expect(screen.getByRole('dialog', { name: '角色编辑' }).parentElement).toBe(portalHost);
  });

  it('前端校验失败时绑定字段错误并聚焦首个字段', async () => {
    const { notify, onSave } = renderEditor({
      persona: { ...persona, id: '' }
    });

    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('请填写角色 ID。');
    await waitFor(() => expect(document.querySelector('[name="id"]')).toHaveFocus());
    expect(onSave).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({ title: '角色资料尚未完成', tone: 'warning' })
    );
  });

  it('后端字段错误会映射到表单并保留编辑器用于修正重试', async () => {
    const onSave = vi.fn().mockRejectedValue(
      new ApiError('角色资料不合法。', 400, {
        fieldErrors: { 'persona.character_profile': '角色特征不符合规则。' }
      })
    );
    const notify = vi.fn();
    renderEditor({}, { onSave, notify });

    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('角色特征不符合规则。');
    await waitFor(() => expect(document.querySelector('[name="character_profile"]')).toHaveFocus());
    expect(screen.getByRole('dialog', { name: '角色编辑' })).toBeVisible();
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({ title: '创建角色失败', tone: 'error' })
    );
  });

  it('角色状态锁定时禁止产生图片上传写入', () => {
    renderEditor({}, { busy: true });

    const fileInputs = Array.from(document.querySelectorAll<HTMLInputElement>('input[type="file"]'));
    const portraitSlot = document.querySelector<HTMLElement>('.persona-image-slot');
    expect(fileInputs).toHaveLength(3);
    for (const fileInput of fileInputs) expect(fileInput).toBeDisabled();
    fireEvent.drop(portraitSlot!, {
      dataTransfer: {
        files: [new File(['image'], 'alice.png', { type: 'image/png' })]
      }
    });
    expect(api.uploadPersonaImage).not.toHaveBeenCalled();
  });

  it('上传头像只更新头像槽，不覆盖立绘与背景', async () => {
    api.uploadPersonaImage.mockResolvedValue({ url: '/api/assets/uploaded/avatar.webp' });
    const setEditor = vi.fn();
    const initialEditor: PersonaEditorState = {
      open: true,
      mode: 'edit',
      persona,
      visualPackDraft: {
        portrait_path: '/api/assets/uploaded/portrait.webp',
        background_path: '/api/assets/uploaded/background.webp',
        avatar_path: ''
      },
      visualDirty: false
    };
    renderEditor(initialEditor, { setEditor });

    const fileInputs = Array.from(document.querySelectorAll<HTMLInputElement>('input[type="file"]'));
    fireEvent.change(fileInputs[1], {
      target: { files: [new File(['avatar'], 'avatar.png', { type: 'image/png' })] }
    });

    await waitFor(() => expect(setEditor).toHaveBeenCalled());
    const updater = setEditor.mock.calls[0][0] as (state: PersonaEditorState) => PersonaEditorState;
    const next = updater(initialEditor);
    expect(next.visualPackDraft).toMatchObject({
      portrait_path: '/api/assets/uploaded/portrait.webp',
      background_path: '/api/assets/uploaded/background.webp',
      avatar_path: '/api/assets/uploaded/avatar.webp'
    });
    expect(next.visualDirty).toBe(true);
  });

  it('无图角色也会保存显式主题设置', async () => {
    const { onSave } = renderEditor({
      mode: 'edit',
      visualPackDraft: {
        portrait_path: '',
        background_path: '',
        avatar_path: '',
        theme_color: '#bf5268',
        theme_mode: 'light'
      },
      visualDirty: true
    });

    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
    expect(onSave).toHaveBeenCalledWith(
      persona,
      'edit',
      expect.objectContaining({
        portrait_path: '',
        background_path: undefined,
        avatar_path: undefined,
        theme_color: '#bf5268',
        theme_mode: 'light'
      })
    );
  });

  it('角色编辑器可以写入 Skill 与 MCP 运行策略', () => {
    const updateEditor = vi.fn();
    renderEditor({}, { updateEditor });

    fireEvent.change(screen.getByLabelText('Skill 策略'), {
      target: { value: 'allow_list' }
    });
    fireEvent.change(screen.getByLabelText('MCP Server 白名单'), {
      target: { value: 'docs, search' }
    });

    expect(updateEditor).toHaveBeenCalledWith('skill_policy', {
      mode: 'allow_list',
      allowed_skills: []
    });
    expect(updateEditor).toHaveBeenCalledWith('mcp_policy', {
      mode: 'inherit',
      allowed_servers: ['docs', 'search']
    });
  });

  it('保留失效模型引用并提供显式修复，同时允许填写角色音色', () => {
    const updateEditor = vi.fn();
    renderEditor(
      {
        persona: {
          ...persona,
          preferred_model_ref: {
            provider_id: 'removed-provider',
            model_id: 'removed-model'
          }
        }
      },
      { updateEditor, modelCatalog }
    );

    expect(screen.getByRole('status')).toHaveTextContent('引用会继续保留');
    fireEvent.change(screen.getByLabelText('偏好聊天模型'), {
      target: { value: JSON.stringify(['deepseek', 'deepseek-v4-pro']) }
    });
    fireEvent.change(screen.getByLabelText('偏好音色 ID'), {
      target: { value: 'persona-voice' }
    });

    expect(updateEditor).toHaveBeenCalledWith('preferred_model_ref', {
      provider_id: 'deepseek',
      model_id: 'deepseek-v4-pro'
    });
    expect(updateEditor).toHaveBeenCalledWith('preferred_voice_id', 'persona-voice');
  });

  it('存在但不支持聊天的模型仍以失效引用展示', () => {
    renderEditor(
      {
        persona: {
          ...persona,
          preferred_model_ref: {
            provider_id: 'deepseek',
            model_id: 'audio-only-model'
          }
        }
      },
      {
        modelCatalog: {
          ...modelCatalog,
          models: [
            ...modelCatalog.models,
            {
              ...modelCatalog.models[0],
              id: 'deepseek:audio-only-model',
              model: 'audio-only-model',
              name: '仅音频模型',
              functions: ['audio_understanding'],
              capabilities: ['audio_understanding']
            }
          ]
        }
      }
    );

    expect(screen.getByLabelText('偏好聊天模型')).toHaveValue(
      JSON.stringify(['deepseek', 'audio-only-model'])
    );
    expect(screen.getByRole('status')).toHaveTextContent('引用会继续保留');
  });
});
