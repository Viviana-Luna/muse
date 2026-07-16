import type { ReactElement } from 'react';
import * as AlertDialog from '@radix-ui/react-alert-dialog';

interface ConfirmDialogProps {
  children: ReactElement;
  title: string;
  description: string;
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: 'default' | 'danger';
  onConfirm: () => void;
}

export function ConfirmDialog({
  children,
  title,
  description,
  confirmLabel = '确认',
  cancelLabel = '取消',
  tone = 'default',
  onConfirm
}: ConfirmDialogProps) {
  return (
    <AlertDialog.Root>
      <AlertDialog.Trigger asChild>{children}</AlertDialog.Trigger>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="confirm-dialog-overlay" />
        <AlertDialog.Content className="confirm-dialog-content">
          <span className={`confirm-dialog-mark ${tone}`} aria-hidden="true">
            !
          </span>
          <AlertDialog.Title className="confirm-dialog-title">{title}</AlertDialog.Title>
          <AlertDialog.Description className="confirm-dialog-description">
            {description}
          </AlertDialog.Description>
          <div className="confirm-dialog-actions">
            <AlertDialog.Cancel asChild>
              <button type="button">{cancelLabel}</button>
            </AlertDialog.Cancel>
            <AlertDialog.Action asChild>
              <button
                type="button"
                className={tone === 'danger' ? 'danger' : 'accent'}
                onClick={onConfirm}
              >
                {confirmLabel}
              </button>
            </AlertDialog.Action>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
