import { useEffect, useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Bot, Check, ChevronDown, Hand, ShieldAlert } from 'lucide-react';

import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import type { RuntimeApprovalModePreset } from '@/types';

interface ApprovalModePickerProps {
  value: RuntimeApprovalModePreset;
  switching?: boolean;
  disabled?: boolean;
  disabledReason?: string;
  onChange: (value: RuntimeApprovalModePreset) => void | Promise<void>;
}

const MODE_LABELS: Record<RuntimeApprovalModePreset, string> = {
  manual: '手动审批',
  auto: 'AUTO 模式',
  yolo: 'YOLO 模式'
};

export function ApprovalModePicker({
  value,
  switching = false,
  disabled = false,
  disabledReason,
  onChange
}: ApprovalModePickerProps) {
  const [open, setOpen] = useState(false);
  const [menuStyle, setMenuStyle] = useState<{
    bottom: number;
    left: number;
    width: number;
    maxHeight: number;
  } | null>(null);
  const titleId = useId();
  const menuId = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const blocked = disabled || switching;

  useEffect(() => {
    if (blocked) setOpen(false);
  }, [blocked]);

  useEffect(() => {
    if (!open) return;
    const syncPosition = () => {
      const trigger = triggerRef.current;
      if (!trigger) return;
      const rect = trigger.getBoundingClientRect();
      const horizontalInset = 12;
      const width = Math.min(374, window.innerWidth - horizontalInset * 2);
      setMenuStyle({
        bottom: window.innerHeight - rect.top + 8,
        left: Math.min(
          Math.max(horizontalInset, rect.left),
          window.innerWidth - width - horizontalInset
        ),
        width,
        maxHeight: Math.max(180, rect.top - 20)
      });
    };
    const closeOnOutsidePointer = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (triggerRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      if (target instanceof Element && target.closest('[role="alertdialog"]')) return;
      setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    syncPosition();
    window.addEventListener('resize', syncPosition);
    window.addEventListener('scroll', syncPosition, true);
    window.addEventListener('pointerdown', closeOnOutsidePointer);
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('resize', syncPosition);
      window.removeEventListener('scroll', syncPosition, true);
      window.removeEventListener('pointerdown', closeOnOutsidePointer);
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  useEffect(() => {
    if (!open || !menuStyle) return;
    menuRef.current
      ?.querySelector<HTMLButtonElement>('[role="menuitemradio"][aria-checked="true"]')
      ?.focus();
  }, [menuStyle, open]);

  const select = async (preset: RuntimeApprovalModePreset) => {
    if (preset === value || switching) {
      setOpen(false);
      return;
    }
    await onChange(preset);
    setOpen(false);
  };

  const onMenuKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    const items = Array.from(
      menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]') ?? []
    ).filter((item) => !item.disabled);
    if (items.length === 0) return;
    event.preventDefault();
    const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
    const nextIndex =
      event.key === 'Home'
        ? 0
        : event.key === 'End'
          ? items.length - 1
          : event.key === 'ArrowDown'
            ? (currentIndex + 1 + items.length) % items.length
            : (currentIndex - 1 + items.length) % items.length;
    items[nextIndex]?.focus();
  };

  const menu =
    open && menuStyle
      ? createPortal(
          <div
            ref={menuRef}
            id={menuId}
            className="approval-mode-popover"
            role="menu"
            aria-labelledby={titleId}
            style={menuStyle}
            onKeyDown={onMenuKeyDown}
          >
            <header>
              <span id={titleId}>应如何批准 Muse 操作？</span>
              <small>当前会话</small>
            </header>
            <div className="approval-mode-menu-options">
              <button
                type="button"
                role="menuitemradio"
                aria-checked={value === 'manual'}
                className={value === 'manual' ? 'is-active' : ''}
                disabled={switching}
                onClick={() => void select('manual')}
              >
                <Hand aria-hidden="true" />
                <span>
                  <strong>手动审批</strong>
                  <small>需要审批的动作始终由你确认</small>
                </span>
                {value === 'manual' && <Check className="approval-mode-check" aria-hidden="true" />}
              </button>
              <button
                type="button"
                role="menuitemradio"
                aria-checked={value === 'auto'}
                className={value === 'auto' ? 'is-active' : ''}
                disabled={switching}
                onClick={() => void select('auto')}
              >
                <Bot aria-hidden="true" />
                <span>
                  <strong>AUTO 模式</strong>
                  <small>由隔离审查器判断需审批的风险操作</small>
                </span>
                {value === 'auto' && <Check className="approval-mode-check" aria-hidden="true" />}
              </button>
              <ConfirmDialog
                title="开启当前会话的 YOLO 模式？"
                description="YOLO 会跳过普通工具审批并允许访问工作区外路径。它只在本次应用运行期有效，重启或恢复旧会话会回到手动审批；MCP 不受此模式放宽。"
                confirmLabel="开启 YOLO"
                cancelLabel="继续保持限制"
                tone="danger"
                onConfirm={() => void select('yolo')}
              >
                <button
                  type="button"
                  role="menuitemradio"
                  aria-checked={value === 'yolo'}
                  className={value === 'yolo' ? 'is-active is-yolo' : 'is-yolo'}
                  disabled={switching}
                >
                  <ShieldAlert aria-hidden="true" />
                  <span>
                    <strong>YOLO 模式</strong>
                    <small>跳过普通审批并启用完全访问</small>
                  </span>
                  {value === 'yolo' && <Check className="approval-mode-check" aria-hidden="true" />}
                </button>
              </ConfirmDialog>
            </div>
            <footer>MCP 仍遵守独立安全策略</footer>
          </div>,
          triggerRef.current?.closest<HTMLElement>('.app-shell') ?? document.body
        )
      : null;

  return (
    <div className="approval-mode-picker">
      <button
        ref={triggerRef}
        type="button"
        className={`approval-mode-trigger is-${value}`}
        disabled={blocked}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={menuId}
        title={disabledReason || '切换当前会话的审批模式'}
        onClick={() => setOpen((current) => !current)}
      >
        {value === 'manual' ? (
          <Hand aria-hidden="true" />
        ) : value === 'auto' ? (
          <Bot aria-hidden="true" />
        ) : (
          <ShieldAlert aria-hidden="true" />
        )}
        <span>{switching ? '切换中…' : MODE_LABELS[value]}</span>
        <ChevronDown aria-hidden="true" />
      </button>
      {menu}
    </div>
  );
}
