import { useEffect, useRef, useState } from 'react';
import { Copy, Download, FileDown, MoreHorizontal, Pencil, Trash2 } from 'lucide-react';

import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import type { PersonaDeletionImpactResponse } from '@/types';

interface PersonaActionsMenuProps {
  name: string;
  busy: boolean;
  onEdit: () => void;
  onCopy: () => void;
  onExportFull: () => void;
  onExportLight: () => void;
  onDelete: () => void;
  loadDeletionImpact?: () => Promise<PersonaDeletionImpactResponse>;
}

export function PersonaActionsMenu({
  name,
  busy,
  onEdit,
  onCopy,
  onExportFull,
  onExportLight,
  onDelete,
  loadDeletionImpact
}: PersonaActionsMenuProps) {
  const [open, setOpen] = useState(false);
  const [deletionImpact, setDeletionImpact] = useState<PersonaDeletionImpactResponse | null>(null);
  const [impactLoading, setImpactLoading] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const root = rootRef.current;
    const items = () => Array.from(root?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    window.requestAnimationFrame(() => items()[0]?.focus());

    function closeAndRestoreFocus() {
      setOpen(false);
      window.requestAnimationFrame(() => triggerRef.current?.focus());
    }

    function handlePointerDown(event: PointerEvent) {
      if (!(event.target instanceof Node) || root?.contains(event.target)) return;
      if (
        event.target instanceof Element &&
        event.target.closest('.confirm-dialog-overlay, .confirm-dialog-content')
      ) {
        return;
      }
      setOpen(false);
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        closeAndRestoreFocus();
        return;
      }
      if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
      const menuItems = items();
      if (menuItems.length === 0) return;
      event.preventDefault();
      const currentIndex = menuItems.indexOf(document.activeElement as HTMLElement);
      const direction = event.key === 'ArrowDown' ? 1 : -1;
      const nextIndex = (currentIndex + direction + menuItems.length) % menuItems.length;
      menuItems[nextIndex]?.focus();
    }

    document.addEventListener('pointerdown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('pointerdown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [open]);

  function run(action: () => void) {
    setOpen(false);
    action();
  }

  return (
    <div className="persona-actions-menu" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className="persona-actions-trigger"
        aria-label={`${name}的更多操作`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <MoreHorizontal aria-hidden="true" />
      </button>
      {open && (
        <div className="persona-actions-popover" role="menu" aria-label={`${name}的角色操作`}>
          <button type="button" role="menuitem" onClick={() => run(onEdit)}>
            <Pencil aria-hidden="true" />
            编辑角色
          </button>
          <button type="button" role="menuitem" onClick={() => run(onCopy)}>
            <Copy aria-hidden="true" />
            创建副本
          </button>
          <button type="button" role="menuitem" onClick={() => run(onExportFull)}>
            <Download aria-hidden="true" />
            导出完整角色卡
          </button>
          <button type="button" role="menuitem" onClick={() => run(onExportLight)}>
            <FileDown aria-hidden="true" />
            导出轻量角色卡
          </button>
          <ConfirmDialog
            title={`删除角色“${name}”？`}
            description={
              impactLoading
                ? '正在核对关联会话…'
                : deletionImpact
                  ? `角色配置将被删除；${deletionImpact.associated_session_count} 个关联会话会保留为只读历史。此操作无法撤销。`
                  : '角色配置将从本地存储中删除；关联会话会保留为只读历史。此操作无法撤销。'
            }
            confirmLabel="删除角色"
            tone="danger"
            onConfirm={() => {
              setOpen(false);
              onDelete();
            }}
          >
            <button
              type="button"
              role="menuitem"
              className="danger"
              disabled={busy}
              onClick={() => {
                if (!loadDeletionImpact) return;
                setImpactLoading(true);
                setDeletionImpact(null);
                void loadDeletionImpact()
                  .then(setDeletionImpact)
                  .catch(() => setDeletionImpact(null))
                  .finally(() => setImpactLoading(false));
              }}
            >
              <Trash2 aria-hidden="true" />
              删除角色
            </button>
          </ConfirmDialog>
        </div>
      )}
    </div>
  );
}
