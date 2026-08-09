import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  fetchAppearancePreferences: vi.fn(),
  fetchActivePersona: vi.fn(),
  fetchHistory: vi.fn(),
  fetchModelInfo: vi.fn(),
  fetchPersonas: vi.fn(),
  fetchRuntimeApprovalMode: vi.fn(),
  fetchRuntimeContextSnapshot: vi.fn(),
  fetchRuntimeMode: vi.fn(),
  fetchRuntimeSessions: vi.fn(),
  fetchRuntimeState: vi.fn(),
  fetchRuntimeTodos: vi.fn(),
  fetchRuntimeTokenUsage: vi.fn(),
  fetchVoiceCapabilities: vi.fn()
}));

const client = vi.hoisted(() => ({
  reportDesktopReady: vi.fn(),
  formatApiErrorMessage: vi.fn((_error: unknown, fallback: string) => fallback)
}));

vi.mock('@/api', () => api);
vi.mock('@/api/client', () => client);

import { App } from './MuseApp';

describe('MuseApp 角色导入意图', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.history.replaceState({}, '', '/');
    client.reportDesktopReady.mockResolvedValue(undefined);
    api.fetchAppearancePreferences.mockResolvedValue({
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
    api.fetchRuntimeState.mockResolvedValue({
      state_revision: 1,
      active_persona_id: null,
      active_conversation_id: 'default',
      mode: 'focus',
      focus_phase: 'build',
      busy_turn: null,
      exclusive_operation: null,
      usage_summary: {},
      context_summary: {}
    });
    api.fetchPersonas.mockResolvedValue({ personas: [], active_persona_id: null });
    api.fetchActivePersona.mockResolvedValue({
      active_persona: null,
      active_persona_id: null,
      visual_pack: null,
      state_revision: 1
    });
    api.fetchRuntimeSessions.mockResolvedValue({
      sessions: [],
      active_conversation_id: 'default',
      status: 'ok'
    });
    api.fetchRuntimeTodos.mockResolvedValue({ todos: [], status: 'ok' });
    api.fetchHistory.mockResolvedValue([]);
    api.fetchRuntimeTokenUsage.mockResolvedValue({
      conversation_id: 'default',
      range: 'day',
      to: '',
      records: 0,
      totals: {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        reasoning_tokens: 0,
        server_tool_tokens: 0,
        total_tokens: 0
      },
      by_source: [],
      by_model: [],
      items: [],
      status: 'ok'
    });
    api.fetchRuntimeContextSnapshot.mockResolvedValue({
      conversation_id: 'default',
      snapshot: null,
      status: 'ok'
    });
    api.fetchRuntimeApprovalMode.mockResolvedValue({
      conversation_id: 'default',
      preset: 'manual',
      approval_policy: 'on_request',
      approvals_reviewer: 'user',
      permission_profile: 'workspace_write',
      revision: 0,
      status: 'ok'
    });
    api.fetchModelInfo.mockResolvedValue({ provider: 'mock', model: 'model' });
    api.fetchRuntimeMode.mockResolvedValue({
      mode: 'focus',
      focus_phase: 'build',
      tool_preset: 'focus_build',
      status: 'ok'
    });
    api.fetchVoiceCapabilities.mockResolvedValue({
      tts: false,
      speech_recognition: false
    });
  });

  afterEach(() => cleanup());

  it('冷启动期间只展示全局加载页，不提前暴露应用工作区', () => {
    api.fetchRuntimeState.mockReturnValue(new Promise(() => undefined));

    render(<App />);

    expect(screen.getByRole('status', { name: '正在加载 Muse' })).toBeInTheDocument();
    expect(screen.queryByLabelText('Muse 功能栏')).not.toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: '暂无角色' })).not.toBeInTheDocument();
  });

  it('只把已保存的半透明背景可见度应用到画布并标记材质状态', async () => {
    api.fetchAppearancePreferences.mockResolvedValue({
      schema_version: 1,
      appearance: {
        theme: 'system',
        background_theme: 'dark',
        language: 'zh-CN',
        background_blur: 18,
        background_opacity: 0.72,
        motion_level: 'full'
      },
      diagnostics: []
    });

    const { container } = render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    await waitFor(() => {
      const root = container.querySelector<HTMLElement>('main.runtime-shell');
      expect(root?.style.getPropertyValue('--app-background-opacity')).toBe('0.72');
      expect(root).toHaveClass('background-translucent');
      expect(root).not.toHaveClass('background-solid');
    });
  });

  it('实色背景使用独立材质状态且保持画布不透明', async () => {
    const { container } = render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    const root = container.querySelector<HTMLElement>('main.runtime-shell');
    expect(root?.style.getPropertyValue('--app-background-opacity')).toBe('1');
    expect(root).toHaveClass('background-solid');
    expect(root).not.toHaveClass('background-translucent');
  });

  it('已保存的浅色主题覆盖角色展示包的默认明暗结果', async () => {
    api.fetchAppearancePreferences.mockResolvedValue({
      schema_version: 1,
      appearance: {
        theme: 'light',
        background_theme: 'dark',
        language: 'zh-CN',
        background_blur: 18,
        background_opacity: 1,
        motion_level: 'full'
      },
      diagnostics: []
    });

    const { container } = render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    await waitFor(() => {
      const root = container.querySelector<HTMLElement>('main.runtime-shell');
      expect(root).toHaveClass('theme-light');
      expect(root).not.toHaveClass('theme-dark');
      expect(root).toHaveClass('background-theme-dark');
    });
  });

  it('浅色背景不改变已保存的深色界面主题', async () => {
    api.fetchAppearancePreferences.mockResolvedValue({
      schema_version: 1,
      appearance: {
        theme: 'dark',
        background_theme: 'light',
        language: 'zh-CN',
        background_blur: 18,
        background_opacity: 1,
        motion_level: 'full'
      },
      diagnostics: []
    });

    const { container } = render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    await waitFor(() => {
      const root = container.querySelector<HTMLElement>('main.runtime-shell');
      expect(root).toHaveClass('theme-dark');
      expect(root).toHaveClass('background-theme-light');
      expect(root).not.toHaveClass('theme-light');
    });
  });

  it('从空态进入角色库后打开可审阅的导入流程，不直接弹出系统文件框', async () => {
    const inputClick = vi
      .spyOn(HTMLInputElement.prototype, 'click')
      .mockImplementation(() => undefined);
    render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    fireEvent.click(screen.getByRole('button', { name: '导入角色卡' }));

    expect(await screen.findByRole('heading', { name: '角色库' })).toBeInTheDocument();
    const dialog = await screen.findByRole('dialog', { name: '导入角色卡' });
    const fileInput = dialog.querySelector<HTMLInputElement>('input[type="file"]');
    expect(fileInput).not.toBeNull();
    expect(inputClick).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: /^选择角色卡文件/ }));
    await waitFor(() => expect(inputClick).toHaveBeenCalledOnce());
    expect(inputClick.mock.instances[0]).toBe(fileInput);
    inputClick.mockRestore();
  });

  it('会话栏目使用独立管理页面，不再把历史抽屉叠到聊天工作区', async () => {
    render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    fireEvent.click(screen.getByRole('button', { name: '会话' }));

    expect(await screen.findByRole('region', { name: '会话管理' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: '会话' })).toBeInTheDocument();
    expect(screen.queryByLabelText('历史会话列表')).not.toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: '暂无角色' })).not.toBeInTheDocument();
  });

  it('顶部当前会话入口向下展开聊天记录并支持直接选择', async () => {
    render(<App />);
    await screen.findByRole('heading', { name: '暂无角色' });

    fireEvent.click(screen.getByRole('button', { name: '展开聊天记录：新对话' }));

    const list = await screen.findByRole('listbox', { name: '聊天记录' });
    const currentSession = list.querySelector<HTMLElement>('[role="option"]');
    expect(currentSession).not.toBeNull();
    expect(currentSession).toHaveTextContent('新对话');
    expect(currentSession).toHaveAttribute('aria-selected', 'true');

    fireEvent.click(currentSession!);
    await waitFor(() => expect(screen.queryByRole('listbox', { name: '聊天记录' })).not.toBeInTheDocument());
  });

  it('顶部当前角色入口展开角色列表，不跳转到角色管理页', async () => {
    api.fetchPersonas.mockResolvedValue({
      personas: [
        {
          id: 'alice',
          name: '爱丽丝',
          summary: '安静而敏锐',
          default_visual_pack_id: 'default',
          author: 'Muse',
          version: '1.0.0',
          visual_preview: { avatar_path: null, portrait_path: null }
        },
        {
          id: 'bob',
          name: '鲍勃',
          summary: '可靠的同行者',
          default_visual_pack_id: 'default',
          author: 'Muse',
          version: '1.0.0',
          visual_preview: { avatar_path: null, portrait_path: null }
        }
      ],
      active_persona_id: null
    });

    render(<App />);
    const selector = await screen.findByRole('button', {
      name: '切换当前角色：未选择角色'
    });
    await waitFor(() => expect(selector).toBeEnabled());

    fireEvent.click(selector);

    const list = await screen.findByRole('listbox', { name: '角色' });
    expect(list).toHaveTextContent('爱丽丝');
    expect(list).toHaveTextContent('鲍勃');
    expect(screen.queryByRole('heading', { name: '角色库' })).not.toBeInTheDocument();
  });
});
