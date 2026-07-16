import { useRef, useState } from 'react';

export type AppToastTone = 'info' | 'success' | 'warning' | 'error';

export interface AppToastInput {
  title: string;
  description?: string;
  tone?: AppToastTone;
  duration?: number;
}

export interface AppToastItem extends AppToastInput {
  id: number;
  tone: AppToastTone;
}

const MAX_VISIBLE_TOASTS = 4;
export const DEFAULT_TOAST_DURATIONS: Record<AppToastTone, number> = {
  success: 3200,
  info: 4500,
  warning: 6000,
  error: 8000
};

export function useAppToast() {
  const [toasts, setToasts] = useState<AppToastItem[]>([]);
  const sequenceRef = useRef(0);

  function notify(input: AppToastInput) {
    sequenceRef.current += 1;
    const tone = input.tone ?? 'info';
    const nextToast: AppToastItem = {
      ...input,
      id: sequenceRef.current,
      tone,
      duration: input.duration ?? DEFAULT_TOAST_DURATIONS[tone]
    };
    setToasts((current) => [...current.slice(-(MAX_VISIBLE_TOASTS - 1)), nextToast]);
  }

  function dismissToast(id: number) {
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }

  return { toasts, notify, dismissToast };
}
