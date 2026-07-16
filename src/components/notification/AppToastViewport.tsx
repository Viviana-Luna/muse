import * as Toast from '@radix-ui/react-toast';

import type { AppToastItem } from '@/hooks/useAppToast';

interface AppToastViewportProps {
  toasts: AppToastItem[];
  themeMode: 'dark' | 'light';
  onDismiss: (id: number) => void;
}

const TONE_LABELS: Record<AppToastItem['tone'], string> = {
  info: '提示',
  success: '成功',
  warning: '注意',
  error: '失败'
};

export function AppToastViewport({ toasts, themeMode, onDismiss }: AppToastViewportProps) {
  return (
    <Toast.Provider swipeDirection="right" label="通知">
      {toasts.map((toast) => (
        <Toast.Root
          className={`app-toast theme-${themeMode} ${toast.tone}`}
          duration={toast.duration}
          key={toast.id}
          open
          type="foreground"
          onOpenChange={(open) => {
            if (!open) onDismiss(toast.id);
          }}
        >
          <span className="app-toast-marker" aria-hidden="true" />
          <div className="app-toast-copy">
            <span className="app-toast-tone">{TONE_LABELS[toast.tone]}</span>
            <Toast.Title className="app-toast-title">{toast.title}</Toast.Title>
            {toast.description && (
              <Toast.Description className="app-toast-description">
                {toast.description}
              </Toast.Description>
            )}
          </div>
          <Toast.Close className="app-toast-close" aria-label="关闭通知">
            ×
          </Toast.Close>
        </Toast.Root>
      ))}
      <Toast.Viewport className="app-toast-viewport" />
    </Toast.Provider>
  );
}
