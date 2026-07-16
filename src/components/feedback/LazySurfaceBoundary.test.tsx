import { useState } from 'react';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { LazySurfaceBoundary } from './LazySurfaceBoundary';

function FailingSurface({ fail }: { fail: boolean }) {
  if (fail) throw new Error('chunk failed');
  return <div>设置内容已恢复</div>;
}

function Harness() {
  const [fail, setFail] = useState(true);
  return (
    <LazySurfaceBoundary label="设置中心" onRetry={() => setFail(false)}>
      <FailingSurface fail={fail} />
    </LazySurfaceBoundary>
  );
}

function LoadingHarness({ onClose }: { onClose: () => void }) {
  const [open, setOpen] = useState(false);

  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>打开设置</button>
      {open && (
        <LazySurfaceBoundary
          label="设置中心"
          onRetry={vi.fn()}
          onClose={() => {
            onClose();
            setOpen(false);
          }}
        >
          <section className="modal-shell" role="status" aria-label="正在加载设置">
            <div>正在加载设置中心…</div>
          </section>
        </LazySurfaceBoundary>
      )}
    </>
  );
}

function ErrorHarness({ onClose }: { onClose: () => void }) {
  return (
    <>
      <button type="button">背景操作</button>
      <LazySurfaceBoundary label="设置中心" onRetry={vi.fn()} onClose={onClose}>
        <FailingSurface fail />
      </LazySurfaceBoundary>
    </>
  );
}

describe('LazySurfaceBoundary', () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('局部捕获加载失败并在原位置重试', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    render(<Harness />);

    expect(screen.getByRole('alert', { name: '设置中心加载失败' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '重新加载' }));
    expect(screen.getByText('设置内容已恢复')).toBeInTheDocument();
  });

  it('加载表面捕获焦点、屏蔽背景并在 Escape 关闭后恢复焦点', async () => {
    const onClose = vi.fn();
    render(<LoadingHarness onClose={onClose} />);

    const opener = screen.getByRole('button', { name: '打开设置' });
    opener.focus();
    fireEvent.click(opener);

    const status = screen.getByRole('status', { name: '正在加载设置', hidden: true });
    const surface = status.parentElement;
    expect(surface).toHaveAttribute('data-modal-accessibility-surface', 'true');
    await waitFor(() => expect(surface).toHaveFocus());
    expect(opener.inert).toBe(true);

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();
    expect(screen.queryByRole('status', { name: '正在加载设置' })).not.toBeInTheDocument();
    expect(opener.inert).not.toBe(true);
    expect(opener).toHaveFocus();
  });

  it('错误表面将焦点限制在关闭和重试操作内', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    render(<ErrorHarness onClose={vi.fn()} />);

    const background = screen.getByRole('button', { name: '背景操作' });
    const closeButton = screen.getByRole('button', { name: '关闭设置中心', hidden: true });
    const retryButton = screen.getByRole('button', { name: '重新加载', hidden: true });
    await waitFor(() => expect(closeButton).toHaveFocus());
    expect(background.inert).toBe(true);

    retryButton.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(closeButton).toHaveFocus();
  });
});
