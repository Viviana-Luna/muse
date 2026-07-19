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
    vi.useRealTimers();
    Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
  });

  it('使用自绘按钮调用桌面窗口控制命令', async () => {
    render(<AppTitleBar {...titleBarProps()} />);

    fireEvent.click(screen.getByRole('button', { name: '关闭窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '最小化窗口' }));
    fireEvent.click(screen.getByRole('button', { name: '最大化或还原窗口' }));

    await waitFor(() => {
      expect(windowApi.close).toHaveBeenCalledOnce();
      expect(windowApi.minimize).toHaveBeenCalledOnce();
      expect(windowApi.toggleMaximize).toHaveBeenCalledOnce();
    });
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

  it('macOS 使用原生窗口按钮并为其保留固定区域', () => {
    const platform = vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('MacIntel');
    const { container } = render(<AppTitleBar {...titleBarProps()} />);

    expect(screen.queryByRole('button', { name: '关闭窗口' })).not.toBeInTheDocument();
    expect(container.querySelector('.app-titlebar-left')).toHaveClass('native-macos-controls');
    platform.mockRestore();
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
