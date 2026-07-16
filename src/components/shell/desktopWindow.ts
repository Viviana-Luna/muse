import { getCurrentWindow } from '@tauri-apps/api/window';

function isTauriDesktopRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

async function runWindowCommand(
  command: (window: ReturnType<typeof getCurrentWindow>) => Promise<void>
): Promise<void> {
  if (!isTauriDesktopRuntime()) return;
  await command(getCurrentWindow());
}

export function closeDesktopWindow(): Promise<void> {
  return runWindowCommand((window) => window.close());
}

export function minimizeDesktopWindow(): Promise<void> {
  return runWindowCommand((window) => window.minimize());
}

export function toggleDesktopWindowMaximize(): Promise<void> {
  return runWindowCommand((window) => window.toggleMaximize());
}
