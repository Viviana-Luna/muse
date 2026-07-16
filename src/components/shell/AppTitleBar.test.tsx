import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const windowApi = vi.hoisted(() => ({
  close: vi.fn(),
  minimize: vi.fn(),
  toggleMaximize: vi.fn()
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowApi
}));

import { AppTitleBar } from './AppTitleBar';

const conversationContext = {
  title: '雨夜重逢',
  stateLabel: '当前会话',
  contextProgress: 1,
  usageLabel: '1%',
  tokenLabel: '1,200',
  detailLabel: '1.2K / 128K，剩余 126.8K',
  costLabel: 'US$0.0043'
};

describe('AppTitleBar', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    windowApi.close.mockResolvedValue(undefined);
    windowApi.minimize.mockResolvedValue(undefined);
    windowApi.toggleMaximize.mockResolvedValue(undefined);
    Object.defineProperty(window, '__TAURI_INTERNALS__', {
      configurable: true,
      value: {}
    });
  });

  afterEach(() => {
    cleanup();
    Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
  });

  it('使用自绘按钮调用桌面窗口控制命令', async () => {
    render(
      <AppTitleBar
        railCollapsed={false}
        modelLabel="DeepSeek / deepseek-chat"
        conversationContext={conversationContext}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen={false}
        modelSelectorDisabled={false}
        onToggleRail={vi.fn()}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={vi.fn()}
      />
    );

    fireEvent.click(screen.getByRole('button', { name: '关闭窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '最小化窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '最大化或还原窗口' }));

    await waitFor(() => {
      expect(windowApi.close).toHaveBeenCalledOnce();
      expect(windowApi.minimize).toHaveBeenCalledOnce();
      expect(windowApi.toggleMaximize).toHaveBeenCalledOnce();
    });
  });

  it('把当前会话、角色、模型和环形上下文状态集中到标题栏', () => {
    const onOpenSessionSelector = vi.fn();
    const onOpenPersonaSelector = vi.fn();
    const onOpenModelSelector = vi.fn();
    render(
      <AppTitleBar
        railCollapsed={false}
        activePersonaName="爱丽丝"
        modelLabel="DeepSeek / deepseek-chat"
        conversationContext={conversationContext}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen={false}
        modelSelectorDisabled={false}
        onToggleRail={vi.fn()}
        onOpenSessionSelector={onOpenSessionSelector}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={onOpenPersonaSelector}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={onOpenModelSelector}
        onCloseModelSelector={vi.fn()}
      />
    );

    fireEvent.click(screen.getByRole('button', { name: '展开聊天记录：雨夜重逢' }));
    fireEvent.click(screen.getByRole('button', { name: '切换当前角色：爱丽丝' }));
    fireEvent.click(screen.getByRole('button', { name: '切换聊天模型：DeepSeek / deepseek-chat' }));

    expect(onOpenSessionSelector).toHaveBeenCalledOnce();
    expect(onOpenPersonaSelector).toHaveBeenCalledOnce();
    expect(onOpenModelSelector).toHaveBeenCalledOnce();
    expect(screen.getByRole('button', { name: '上下文使用率 1%' })).toBeInTheDocument();
    expect(screen.getByRole('tooltip')).toHaveTextContent('成本US$0.0043');
    expect(screen.getByRole('tooltip')).toHaveTextContent('使用率1%');
    expect(screen.getByRole('tooltip')).toHaveTextContent('Token1,200');
    expect(screen.queryByRole('combobox', { name: '搜索 Muse 功能' })).not.toBeInTheDocument();
  });

  it('未提供成本时不在上下文详情中渲染成本行', () => {
    render(
      <AppTitleBar
        railCollapsed={false}
        modelLabel="火山方舟 Agent Plan / glm-5.2"
        conversationContext={{ ...conversationContext, costLabel: undefined }}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen={false}
        modelSelectorDisabled={false}
        onToggleRail={vi.fn()}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={vi.fn()}
      />
    );

    expect(screen.getByRole('tooltip')).not.toHaveTextContent('成本');
    expect(screen.getByRole('tooltip')).toHaveTextContent('Token1,200');
  });

  it('macOS 使用原生窗口按钮并为其保留固定区域', () => {
    const platform = vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('MacIntel');
    const { container } = render(
      <AppTitleBar
        railCollapsed={false}
        modelLabel="DeepSeek / deepseek-chat"
        conversationContext={conversationContext}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen={false}
        modelSelectorDisabled={false}
        onToggleRail={vi.fn()}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={vi.fn()}
      />
    );

    expect(screen.queryByRole('button', { name: '关闭窗口' })).not.toBeInTheDocument();
    expect(container.querySelector('.app-titlebar-left')).toHaveClass('native-macos-controls');
    platform.mockRestore();
  });

  it('侧栏按钮暴露当前折叠状态并触发切换', () => {
    const onToggleRail = vi.fn();
    render(
      <AppTitleBar
        railCollapsed
        modelLabel="模型未配置"
        conversationContext={conversationContext}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen
        modelSelectorDisabled={false}
        onToggleRail={onToggleRail}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={vi.fn()}
      />
    );

    const toggle = screen.getByRole('button', { name: '展开功能栏' });
    expect(toggle).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(toggle);
    expect(onToggleRail).toHaveBeenCalledOnce();
  });

  it('模型弹出菜单支持选中项聚焦、Escape 和点击外部关闭', async () => {
    const onCloseModelSelector = vi.fn();
    render(
      <AppTitleBar
        railCollapsed={false}
        modelLabel="DeepSeek / deepseek-chat"
        conversationContext={conversationContext}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen
        modelSelectorDisabled={false}
        modelPickerContent={
          <div role="listbox" aria-label="聊天模型">
            <button type="button" role="option" aria-selected="true">DeepSeek Chat</button>
          </div>
        }
        onToggleRail={vi.fn()}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={onCloseModelSelector}
      />
    );

    await waitFor(() => expect(screen.getByRole('option', { name: 'DeepSeek Chat' })).toHaveFocus());
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onCloseModelSelector).toHaveBeenCalledOnce();

    fireEvent.pointerDown(document.body);
    expect(onCloseModelSelector).toHaveBeenCalledTimes(2);
  });

  it('角色弹出菜单支持选中项聚焦、Escape 和点击外部关闭', async () => {
    const onClosePersonaSelector = vi.fn();
    render(
      <AppTitleBar
        railCollapsed={false}
        activePersonaName="爱丽丝"
        modelLabel="DeepSeek / deepseek-chat"
        conversationContext={conversationContext}
        sessionSelectorOpen={false}
        sessionSelectorDisabled={false}
        personaSelectorOpen
        personaSelectorDisabled={false}
        personaPickerContent={
          <div role="listbox" aria-label="角色">
            <button type="button" role="option" aria-selected="true">爱丽丝</button>
          </div>
        }
        modelSelectorOpen={false}
        modelSelectorDisabled={false}
        onToggleRail={vi.fn()}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={vi.fn()}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={onClosePersonaSelector}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={vi.fn()}
      />
    );

    await waitFor(() => expect(screen.getByRole('option', { name: '爱丽丝' })).toHaveFocus());
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClosePersonaSelector).toHaveBeenCalledOnce();

    fireEvent.pointerDown(document.body);
    expect(onClosePersonaSelector).toHaveBeenCalledTimes(2);
  });

  it('聊天记录菜单支持选中项聚焦、Escape 和点击外部关闭', async () => {
    const onCloseSessionSelector = vi.fn();
    render(
      <AppTitleBar
        railCollapsed={false}
        modelLabel="DeepSeek / deepseek-chat"
        conversationContext={conversationContext}
        sessionSelectorOpen
        sessionSelectorDisabled={false}
        sessionPickerContent={
          <div role="listbox" aria-label="聊天记录">
            <button type="button" role="option" aria-selected="true">雨夜重逢</button>
          </div>
        }
        personaSelectorOpen={false}
        personaSelectorDisabled={false}
        modelSelectorOpen={false}
        modelSelectorDisabled={false}
        onToggleRail={vi.fn()}
        onOpenSessionSelector={vi.fn()}
        onCloseSessionSelector={onCloseSessionSelector}
        onOpenPersonaSelector={vi.fn()}
        onClosePersonaSelector={vi.fn()}
        onOpenModelSelector={vi.fn()}
        onCloseModelSelector={vi.fn()}
      />
    );

    await waitFor(() => expect(screen.getByRole('option', { name: '雨夜重逢' })).toHaveFocus());
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onCloseSessionSelector).toHaveBeenCalledOnce();

    fireEvent.pointerDown(document.body);
    expect(onCloseSessionSelector).toHaveBeenCalledTimes(2);
  });
});
