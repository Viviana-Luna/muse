import { useEffect, useRef, useState, type ReactNode } from 'react';
import {
  Bot,
  ChevronDown,
  Copy,
  MessageSquareText,
  Minus,
  Square,
  UserRound,
  X
} from 'lucide-react';

import {
  closeDesktopWindow,
  isDesktopWindowMaximized,
  listenDesktopWindowResized,
  minimizeDesktopWindow,
  toggleDesktopWindowMaximize
} from '@/components/shell/desktopWindow';

export interface TitleBarConversationContext {
  title: string;
  stateLabel: string;
  contextProgress: number | null;
  usageLabel: string;
  tokenLabel: string;
  detailLabel: string;
  balanceLabel?: string;
}

function usesNativeMacWindowControls(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /Mac/i.test(navigator.platform || navigator.userAgent);
}

function reportWindowCommandFailure(message: string): (error: unknown) => void {
  return (error) => {
    console.error(message, error);
  };
}

const timeFormatter = new Intl.DateTimeFormat('zh-CN', {
  hour: '2-digit',
  minute: '2-digit',
  hour12: false
});
const dateFormatter = new Intl.DateTimeFormat('zh-CN', {
  month: '2-digit',
  day: '2-digit'
});
const weekdayFormatter = new Intl.DateTimeFormat('zh-CN', { weekday: 'short' });
const accessibleDateTimeFormatter = new Intl.DateTimeFormat('zh-CN', {
  year: 'numeric',
  month: 'long',
  day: 'numeric',
  weekday: 'long',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false
});

// 分钟对齐的整点刷新，避免固定间隔造成的分钟漂移。
function useCurrentDateTime(): {
  dateLabel: string;
  timeLabel: string;
  accessibleLabel: string;
} {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    let timer = 0;
    function scheduleNext() {
      timer = window.setTimeout(() => {
        setNow(new Date());
        scheduleNext();
      }, 60_000 - (Date.now() % 60_000));
    }
    scheduleNext();
    return () => window.clearTimeout(timer);
  }, []);
  return {
    dateLabel: `${dateFormatter.format(now)} ${weekdayFormatter.format(now)}`,
    timeLabel: timeFormatter.format(now),
    accessibleLabel: accessibleDateTimeFormatter.format(now)
  };
}

export function AppTitleBar({
  activePersonaName,
  modelLabel,
  conversationContext,
  sessionSelectorOpen,
  sessionSelectorDisabled,
  sessionPickerContent,
  personaSelectorOpen,
  personaSelectorDisabled,
  personaPickerContent,
  modelSelectorOpen,
  modelSelectorDisabled,
  modelPickerContent,
  onOpenSessionSelector,
  onCloseSessionSelector,
  onOpenPersonaSelector,
  onClosePersonaSelector,
  onOpenModelSelector,
  onCloseModelSelector
}: {
  activePersonaName?: string;
  modelLabel: string;
  conversationContext: TitleBarConversationContext;
  sessionSelectorOpen: boolean;
  sessionSelectorDisabled: boolean;
  sessionPickerContent?: ReactNode;
  personaSelectorOpen: boolean;
  personaSelectorDisabled: boolean;
  personaPickerContent?: ReactNode;
  modelSelectorOpen: boolean;
  modelSelectorDisabled: boolean;
  modelPickerContent?: ReactNode;
  onOpenSessionSelector: () => void;
  onCloseSessionSelector: () => void;
  onOpenPersonaSelector: () => void;
  onClosePersonaSelector: () => void;
  onOpenModelSelector: () => void;
  onCloseModelSelector: () => void;
}) {
  const sessionPickerAnchorRef = useRef<HTMLDivElement>(null);
  const personaPickerAnchorRef = useRef<HTMLDivElement>(null);
  const modelPickerAnchorRef = useRef<HTMLDivElement>(null);
  const [modelProviderName, ...modelNameParts] = modelLabel.split(' / ');
  const modelDisplayName = modelNameParts.join(' / ');
  const personaLabel = activePersonaName || '未选择角色';
  const nativeMacWindowControls = usesNativeMacWindowControls();
  const [windowMaximized, setWindowMaximized] = useState(false);
  const contextProgress = Math.max(0, Math.min(100, conversationContext.contextProgress ?? 0));
  const currentDateTime = useCurrentDateTime();

  useEffect(() => {
    if (nativeMacWindowControls) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;

    function refreshMaximizedState() {
      void isDesktopWindowMaximized()
        .then((maximized) => {
          if (!disposed) setWindowMaximized(maximized);
        })
        .catch(reportWindowCommandFailure('无法读取窗口最大化状态。'));
    }

    refreshMaximizedState();
    void listenDesktopWindowResized(refreshMaximizedState)
      .then((stopListening) => {
        if (disposed) {
          stopListening();
        } else {
          unlisten = stopListening;
        }
      })
      .catch(reportWindowCommandFailure('无法监听窗口尺寸变化。'));

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [nativeMacWindowControls]);

  useEffect(() => {
    if (!sessionSelectorOpen && !personaSelectorOpen && !modelSelectorOpen) return;
    const activeAnchor = sessionSelectorOpen
      ? sessionPickerAnchorRef.current
      : personaSelectorOpen
        ? personaPickerAnchorRef.current
        : modelPickerAnchorRef.current;
    if (!activeAnchor) return;

    const focusFrame = window.requestAnimationFrame(() => {
      activeAnchor.querySelector<HTMLElement>('[role="option"][aria-selected="true"]')?.focus();
    });
    function closeOnOutsidePointer(event: PointerEvent) {
      const target = event.target as Node;
      if (sessionSelectorOpen && !sessionPickerAnchorRef.current?.contains(target)) {
        onCloseSessionSelector();
      }
      if (personaSelectorOpen && !personaPickerAnchorRef.current?.contains(target)) {
        onClosePersonaSelector();
      }
      if (modelSelectorOpen && !modelPickerAnchorRef.current?.contains(target)) {
        onCloseModelSelector();
      }
    }
    function closeOnEscape(event: globalThis.KeyboardEvent) {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      if (sessionSelectorOpen) onCloseSessionSelector();
      if (personaSelectorOpen) onClosePersonaSelector();
      if (modelSelectorOpen) onCloseModelSelector();
    }
    document.addEventListener('pointerdown', closeOnOutsidePointer);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      document.removeEventListener('pointerdown', closeOnOutsidePointer);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [
    modelSelectorOpen,
    onCloseModelSelector,
    onClosePersonaSelector,
    onCloseSessionSelector,
    personaSelectorOpen,
    sessionSelectorOpen
  ]);

  return (
    <header
      className={`app-titlebar ${nativeMacWindowControls ? 'app-titlebar-macos' : 'app-titlebar-windows'}`}
      aria-label="Muse 窗口工具栏"
      data-tauri-drag-region="deep"
    >
      <div
        className={`app-titlebar-left${nativeMacWindowControls ? ' native-macos-controls' : ''}`}
        data-tauri-drag-region="deep"
      />

      <div className="titlebar-context-bar" data-tauri-drag-region="false">
        <div
          className="titlebar-segment"
          ref={sessionPickerAnchorRef}
          data-tauri-drag-region="false"
        >
          <button
            type="button"
            className="titlebar-segment-button titlebar-conversation-selector"
            aria-label={`展开聊天记录：${conversationContext.title}`}
            aria-haspopup="listbox"
            aria-expanded={sessionSelectorOpen}
            title={`${conversationContext.stateLabel}：${conversationContext.title}`}
            disabled={sessionSelectorDisabled}
            onClick={onOpenSessionSelector}
          >
            <MessageSquareText aria-hidden="true" />
            <span>
              <small>{conversationContext.stateLabel}</small>
              <strong>{conversationContext.title}</strong>
            </span>
            <ChevronDown aria-hidden="true" />
          </button>
          {sessionSelectorOpen && sessionPickerContent}
        </div>
        <div
          className="titlebar-segment titlebar-persona-segment"
          ref={personaPickerAnchorRef}
          data-tauri-drag-region="false"
        >
          <button
            type="button"
            className="titlebar-segment-button titlebar-persona-selector"
            aria-label={`切换当前角色：${personaLabel}`}
            aria-haspopup="listbox"
            aria-expanded={personaSelectorOpen}
            title={`当前角色：${personaLabel}`}
            disabled={personaSelectorDisabled}
            onClick={onOpenPersonaSelector}
          >
            <UserRound aria-hidden="true" />
            <span>
              <small>当前角色</small>
              <strong>{personaLabel}</strong>
            </span>
            <ChevronDown aria-hidden="true" />
          </button>
          {personaSelectorOpen && personaPickerContent}
        </div>
        <div
          className="titlebar-segment"
          ref={modelPickerAnchorRef}
          data-tauri-drag-region="false"
        >
          <button
            type="button"
            className="titlebar-segment-button titlebar-model-selector"
            aria-label={`切换聊天模型：${modelLabel}`}
            aria-haspopup="listbox"
            aria-expanded={modelSelectorOpen}
            title={`当前聊天模型：${modelLabel}`}
            disabled={modelSelectorDisabled}
            onClick={onOpenModelSelector}
          >
            <Bot aria-hidden="true" />
            <span>
              <small>聊天模型</small>
              <span className="titlebar-model-identity">
                <strong>{modelProviderName}</strong>
                {modelDisplayName && <em>{modelDisplayName}</em>}
              </span>
            </span>
            <ChevronDown aria-hidden="true" />
          </button>
          {modelSelectorOpen && modelPickerContent}
        </div>

        <section
          className="titlebar-runtime-summary"
          aria-label="当前对话上下文状态"
          data-tauri-drag-region="false"
        >
          <button
            type="button"
            className="titlebar-context-ring-button"
            aria-label={`上下文使用率 ${conversationContext.usageLabel}`}
            aria-describedby="titlebar-context-tooltip"
          >
            <svg viewBox="0 0 36 36" aria-hidden="true">
              <circle className="titlebar-context-ring-track" cx="18" cy="18" r="14" />
              <circle
                className="titlebar-context-ring-value"
                cx="18"
                cy="18"
                r="14"
                pathLength="100"
                style={{ strokeDashoffset: 100 - contextProgress }}
              />
            </svg>
          </button>
          <div id="titlebar-context-tooltip" className="titlebar-context-tooltip" role="tooltip">
            {conversationContext.balanceLabel !== undefined && (
              <span>
                <small>余额</small>
                <strong>{conversationContext.balanceLabel}</strong>
              </span>
            )}
            <span>
              <small>使用率</small>
              <strong>{conversationContext.usageLabel}</strong>
            </span>
            <span>
              <small>Token</small>
              <strong>{conversationContext.tokenLabel}</strong>
            </span>
            <em>{conversationContext.detailLabel}</em>
          </div>
        </section>

        <span
          className="titlebar-clock"
          aria-label={`当前日期和时间：${currentDateTime.accessibleLabel}`}
        >
          <small>{currentDateTime.dateLabel}</small>
          <strong>{currentDateTime.timeLabel}</strong>
        </span>
      </div>

      {!nativeMacWindowControls && (
        <div className="window-controls" aria-label="窗口控制" data-tauri-drag-region="false">
          <button
            type="button"
            className="window-control window-control-minimize"
            aria-label="最小化窗口"
            title="最小化窗口"
            data-tauri-drag-region="false"
            onClick={() =>
              void minimizeDesktopWindow().catch(reportWindowCommandFailure('无法最小化窗口。'))
            }
          >
            <Minus aria-hidden="true" />
          </button>
          <button
            type="button"
            className="window-control window-control-maximize"
            aria-label={windowMaximized ? '还原窗口' : '最大化窗口'}
            title={windowMaximized ? '还原窗口' : '最大化窗口'}
            data-tauri-drag-region="false"
            onClick={() =>
              void toggleDesktopWindowMaximize()
                .then(isDesktopWindowMaximized)
                .then(setWindowMaximized)
                .catch(reportWindowCommandFailure('无法切换窗口大小。'))
            }
          >
            {windowMaximized ? <Copy aria-hidden="true" /> : <Square aria-hidden="true" />}
          </button>
          <button
            type="button"
            className="window-control window-control-close"
            aria-label="关闭窗口"
            title="关闭窗口"
            data-tauri-drag-region="false"
            onClick={() => void closeDesktopWindow().catch(reportWindowCommandFailure('无法关闭窗口。'))}
          >
            <X aria-hidden="true" />
          </button>
        </div>
      )}
    </header>
  );
}
