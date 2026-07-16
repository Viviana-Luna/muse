import { useEffect, useRef, type ReactNode } from 'react';
import {
  Bot,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Maximize2,
  MessageSquareText,
  Minus,
  PanelLeft,
  UserRound,
  X
} from 'lucide-react';

import {
  closeDesktopWindow,
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
  costLabel?: string;
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

export function AppTitleBar({
  railCollapsed,
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
  onToggleRail,
  onOpenSessionSelector,
  onCloseSessionSelector,
  onOpenPersonaSelector,
  onClosePersonaSelector,
  onOpenModelSelector,
  onCloseModelSelector
}: {
  railCollapsed: boolean;
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
  onToggleRail: () => void;
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
  const contextProgress = Math.max(0, Math.min(100, conversationContext.contextProgress ?? 0));

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
    <header className="app-titlebar" aria-label="Muse 窗口工具栏" data-tauri-drag-region="deep">
      <div
        className={`app-titlebar-left${nativeMacWindowControls ? ' native-macos-controls' : ''}`}
        data-tauri-drag-region="false"
      >
        {!nativeMacWindowControls && <div className="window-controls" aria-label="窗口控制">
          <button
            type="button"
            className="window-control window-control-close"
            aria-label="关闭窗口"
            title="关闭窗口"
            onClick={() => void closeDesktopWindow().catch(reportWindowCommandFailure('无法关闭窗口。'))}
          >
            <X aria-hidden="true" />
          </button>
          <button
            type="button"
            className="window-control window-control-minimize"
            aria-label="最小化窗口"
            title="最小化窗口"
            onClick={() =>
              void minimizeDesktopWindow().catch(reportWindowCommandFailure('无法最小化窗口。'))
            }
          >
            <Minus aria-hidden="true" />
          </button>
          <button
            type="button"
            className="window-control window-control-maximize"
            aria-label="最大化或还原窗口"
            title="最大化或还原窗口"
            onClick={() =>
              void toggleDesktopWindowMaximize().catch(
                reportWindowCommandFailure('无法切换窗口大小。')
              )
            }
          >
            <Maximize2 aria-hidden="true" />
          </button>
        </div>}
        <button
          type="button"
          className="titlebar-action titlebar-rail-toggle"
          aria-label={railCollapsed ? '展开功能栏' : '收起功能栏'}
          aria-pressed={railCollapsed}
          title={railCollapsed ? '展开功能栏' : '收起功能栏'}
          onClick={onToggleRail}
        >
          <PanelLeft aria-hidden="true" />
        </button>
        <div className="titlebar-history-actions" aria-label="页面历史">
          <button
            type="button"
            className="titlebar-action"
            aria-label="后退"
            title="后退"
            onClick={() => window.history.back()}
          >
            <ChevronLeft aria-hidden="true" />
          </button>
          <button
            type="button"
            className="titlebar-action"
            aria-label="前进"
            title="前进"
            onClick={() => window.history.forward()}
          >
            <ChevronRight aria-hidden="true" />
          </button>
        </div>
      </div>

      <div
        className="titlebar-conversation-picker-anchor"
        ref={sessionPickerAnchorRef}
        data-tauri-drag-region="false"
      >
        <button
          type="button"
          className="titlebar-context-selector titlebar-conversation-selector"
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

      <div className="titlebar-session-context" aria-label="当前对话环境" data-tauri-drag-region="false">
        <div
          className="titlebar-persona-picker-anchor"
          ref={personaPickerAnchorRef}
          data-tauri-drag-region="false"
        >
          <button
            type="button"
            className="titlebar-context-selector titlebar-persona-selector"
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
        <div className="titlebar-model-picker-anchor" ref={modelPickerAnchorRef}>
          <button
            type="button"
            className="titlebar-context-selector titlebar-model-selector"
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
          {conversationContext.costLabel !== undefined && (
            <span>
              <small>成本</small>
              <strong>{conversationContext.costLabel}</strong>
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

      <div className="titlebar-drag-fill" aria-hidden="true" data-tauri-drag-region="deep" />
    </header>
  );
}
