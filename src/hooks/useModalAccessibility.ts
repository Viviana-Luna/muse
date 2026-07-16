import { useEffect, useRef } from 'react';

const FOCUSABLE_SELECTOR = [
  'button:not([disabled])',
  'a[href]',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])'
].join(',');

const OPEN_MODAL_STACK: symbol[] = [];

/** 为自定义模态统一提供焦点捕获、Escape、背景 inert 和关闭后焦点恢复。 */
export function useModalAccessibility<T extends HTMLElement>(
  open: boolean,
  onRequestClose: () => void
) {
  const dialogRef = useRef<T | null>(null);
  const closeRef = useRef(onRequestClose);
  closeRef.current = onRequestClose;

  useEffect(() => {
    if (!open) return;
    const dialog = dialogRef.current;
    if (!dialog) return;
    const currentDialog = dialog;
    const modalToken = Symbol('modal');
    OPEN_MODAL_STACK.push(modalToken);
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const parent = dialog.parentElement;
    const inertSiblings = parent
      ? Array.from(parent.children).filter(
          (element): element is HTMLElement => element instanceof HTMLElement && element !== dialog
        )
      : [];
    const previousInert = inertSiblings.map((element) => element.inert);
    inertSiblings.forEach((element) => {
      element.inert = true;
    });

    const focusable = () =>
      Array.from(currentDialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
    window.requestAnimationFrame(() => (focusable()[0] ?? currentDialog).focus());

    function handleKeyDown(event: KeyboardEvent) {
      if (OPEN_MODAL_STACK.at(-1) !== modalToken) return;
      if (event.defaultPrevented) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        closeRef.current();
        return;
      }
      if (event.key !== 'Tab') return;
      const items = focusable();
      if (items.length === 0) {
        event.preventDefault();
        currentDialog.focus();
        return;
      }
      const first = items[0];
      const last = items.at(-1)!;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }

    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('keydown', handleKeyDown);
      const stackIndex = OPEN_MODAL_STACK.lastIndexOf(modalToken);
      if (stackIndex >= 0) OPEN_MODAL_STACK.splice(stackIndex, 1);
      inertSiblings.forEach((element, index) => {
        element.inert = previousInert[index];
      });
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, [open]);

  return dialogRef;
}
