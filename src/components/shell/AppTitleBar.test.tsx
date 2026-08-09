import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const windowApi = vi.hoisted(() => ({
  close: vi.fn(),
  isMaximized: vi.fn(),
  minimize: vi.fn(),
  onResized: vi.fn(),
  toggleMaximize: vi.fn(),
  unlistenResized: vi.fn()
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
  balanceLabel: 'CNY 66.23'
};

function titleBarProps(overrides: Record<string, unknown> = {}) {
  return {
    modelLabel: 'DeepSeek / deepseek-chat',
    conversationContext,
    sessionSelectorOpen: false,
    sessionSelectorDisabled: false,
    personaSelectorOpen: false,
    personaSelectorDisabled: false,
    modelSelectorOpen: false,
    modelSelectorDisabled: false,
    onOpenSessionSelector: vi.fn(),
    onCloseSessionSelector: vi.fn(),
    onOpenPersonaSelector: vi.fn(),
    onClosePersonaSelector: vi.fn(),
    onOpenModelSelector: vi.fn(),
    onCloseModelSelector: vi.fn(),
    ...overrides
  };
}

describe('AppTitleBar', () => {
  let platform: ReturnType<typeof vi.spyOn>;
  let resizeListener: (() => void) | undefined;

  beforeEach(() => {
    vi.clearAllMocks();
    resizeListener = undefined;
    windowApi.close.mockResolvedValue(undefined);
    windowApi.isMaximized.mockResolvedValue(false);
    windowApi.minimize.mockResolvedValue(undefined);
    windowApi.onResized.mockImplementation(async (listener: () => void) => {
      resizeListener = listener;
      return windowApi.unlistenResized;
    });
    windowApi.toggleMaximize.mockResolvedValue(undefined);
    platform = vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('Win32');
    Object.defineProperty(window, '__TAURI_INTERNALS__', {
      configurable: true,
      value: {}
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    platform.mockRestore();
    Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
  });

  it('Windows 在右侧按最小化、最大化和关闭顺序渲染可点击控件', async () => {
    const { container } = render(<AppTitleBar {...titleBarProps()} />);

    const titlebar = screen.getByLabelText('Muse 窗口工具栏');
    const controls = screen.getByLabelText('窗口控制');
    const buttons = within(controls).getAllByRole('button');

    expect(titlebar).toHaveClass('app-titlebar-windows');
    expect(titlebar).toHaveAttribute('data-tauri-drag-region', 'deep');
    expect(Array.from(titlebar.children).map((child) => child.className)).toEqual([
      'app-titlebar-left',
      'titlebar-context-bar',
      'window-controls'
    ]);
    expect(container.querySelector('.app-titlebar-left')).toHaveAttribute(
      'data-tauri-drag-region',
      'deep'
    );
    expect(controls).toHaveAttribute('data-tauri-drag-region', 'false');
    expect(buttons.map((button) => button.getAttribute('aria-label'))).toEqual([
      '最小化窗口',
      '最大化窗口',
      '关闭窗口'
    ]);
    for (const button of buttons) {
      expect(button).toHaveAttribute('data-tauri-drag-region', 'false');
    }

    fireEvent.click(screen.getByRole('button', { name: '最小化窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '最大化窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '关闭窗口' }));

    await waitFor(() => {
      expect(windowApi.close).toHaveBeenCalledOnce();
      expect(windowApi.minimize).toHaveBeenCalledOnce();
      expect(windowApi.toggleMaximize).toHaveBeenCalledOnce();
    });
  });

  it('窗口最大化状态变化时切换最大化和还原控件', async () => {
    windowApi.isMaximized.mockResolvedValue(true);
    render(<AppTitleBar {...titleBarProps()} />);

    const restore = await screen.findByRole('button', { name: '还原窗口' });
    expect(restore.querySelector('.lucide-copy')).toBeInTheDocument();
    await waitFor(() => expect(windowApi.onResized).toHaveBeenCalledOnce());

    windowApi.isMaximized.mockResolvedValue(false);
    resizeListener?.();

    const maximize = await screen.findByRole('button', { name: '最大化窗口' });
    expect(maximize.querySelector('.lucide-square')).toBeInTheDocument();
  });

  it('把当前会话、角色、模型、环形上下文状态和时间集中到标题栏', () => {
    const onOpenSessionSelector = vi.fn();
    const onOpenPersonaSelector = vi.fn();
    const onOpenModelSelector = vi.fn();
    render(
      <AppTitleBar
        {...titleBarProps({
          activePersonaName: '爱丽丝',
          onOpenSessionSelector,
          onOpenPersonaSelector,
          onOpenModelSelector
        })}
      />
    );

    fireEvent.click(screen.getByRole('button', { name: '展开聊天记录：雨夜重逢' }));
    fireEvent.click(screen.getByRole('button', { name: '切换当前角色：爱丽丝' }));
    fireEvent.click(screen.getByRole('button', { name: '切换聊天模型：DeepSeek / deepseek-chat' }));

    expect(onOpenSessionSelector).toHaveBeenCalledOnce();
    expect(onOpenPersonaSelector).toHaveBeenCalledOnce();
    expect(onOpenModelSelector).toHaveBeenCalledOnce();
    expect(screen.getByRole('button', { name: '上下文使用率 1%' })).toBeInTheDocument();
    expect(screen.getByRole('tooltip')).toHaveTextContent('余额CNY 66.23');
    expect(screen.getByRole('tooltip')).toHaveTextContent('使用率1%');
    expect(screen.getByRole('tooltip')).toHaveTextContent('Token1,200');
    expect(screen.getByLabelText(/当前日期和时间/)).toHaveTextContent(/\d{2}:\d{2}/);
  });

  it('按本地格式展示日期、星期和时间', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 6, 19, 10, 24));

    render(<AppTitleBar {...titleBarProps()} />);

    const clock = screen.getByLabelText(/当前日期和时间/);
    expect(clock).toHaveTextContent('07/19 周日');
    expect(clock).toHaveTextContent('10:24');
    expect(clock).toHaveAccessibleName(/2026年7月19日星期日.*10:24/);
  });

  it('未提供余额时不在上下文详情中渲染余额行', () => {
    render(
      <AppTitleBar
        {...titleBarProps({
          modelLabel: '火山方舟 Agent Plan / glm-5.2',
          conversationContext: { ...conversationContext, balanceLabel: undefined }
        })}
      />
    );

    expect(screen.getByRole('tooltip')).not.toHaveTextContent('余额');
    expect(screen.getByRole('tooltip')).toHaveTextContent('Token1,200');
  });

  it('macOS 用左侧独立胶囊承载原生窗口按钮并让上下文占满剩余区域', () => {
    platform.mockReturnValue('MacIntel');
    const { container } = render(<AppTitleBar {...titleBarProps()} />);

    const titlebar = screen.getByLabelText('Muse 窗口工具栏');
    const nativeControls = container.querySelector('.app-titlebar-left');
    const contextBar = container.querySelector('.titlebar-context-bar');

    expect(screen.queryByRole('button', { name: '关闭窗口' })).not.toBeInTheDocument();
    expect(screen.queryByLabelText('窗口控制')).not.toBeInTheDocument();
    expect(titlebar).toHaveClass('app-titlebar-macos');
    expect(Array.from(titlebar.children).map((child) => child.className)).toEqual([
      'app-titlebar-left native-macos-controls',
      'titlebar-context-bar'
    ]);
    expect(nativeControls).toHaveClass('native-macos-controls');
    expect(nativeControls).toHaveAttribute('data-tauri-drag-region', 'deep');
    expect(contextBar).toHaveAttribute('data-tauri-drag-region', 'false');
    expect(windowApi.isMaximized).not.toHaveBeenCalled();
    expect(windowApi.onResized).not.toHaveBeenCalled();
  });

  it('模型弹出菜单支持选中项聚焦、Escape 和点击外部关闭', async () => {
    const onCloseModelSelector = vi.fn();
    render(
      <AppTitleBar
        {...titleBarProps({
          modelSelectorOpen: true,
          modelPickerContent: (
            <div role="listbox" aria-label="聊天模型">
              <button type="button" role="option" aria-selected="true">DeepSeek Chat</button>
            </div>
          ),
          onCloseModelSelector
        })}
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
        {...titleBarProps({
          activePersonaName: '爱丽丝',
          personaSelectorOpen: true,
          personaPickerContent: (
            <div role="listbox" aria-label="角色">
              <button type="button" role="option" aria-selected="true">爱丽丝</button>
            </div>
          ),
          onClosePersonaSelector
        })}
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
        {...titleBarProps({
          sessionSelectorOpen: true,
          sessionPickerContent: (
            <div role="listbox" aria-label="聊天记录">
              <button type="button" role="option" aria-selected="true">雨夜重逢</button>
            </div>
          ),
          onCloseSessionSelector
        })}
      />
    );

    await waitFor(() => expect(screen.getByRole('option', { name: '雨夜重逢' })).toHaveFocus());
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onCloseSessionSelector).toHaveBeenCalledOnce();

    fireEvent.pointerDown(document.body);
    expect(onCloseSessionSelector).toHaveBeenCalledTimes(2);
  });
});
