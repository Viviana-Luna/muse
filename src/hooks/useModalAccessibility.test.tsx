import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { useModalAccessibility } from './useModalAccessibility';

afterEach(cleanup);

function Harness({ onClose }: { onClose: () => void }) {
  const [open] = useState(true);
  const ref = useModalAccessibility<HTMLElement>(open, onClose);
  return (
    <main>
      <button type="button">背景操作</button>
      <section ref={ref} tabIndex={-1} role="dialog" aria-modal="true">
        <button type="button">第一个</button>
        <button type="button">最后一个</button>
      </section>
    </main>
  );
}

function ClosingDialog({ onClose }: { onClose: () => void }) {
  const ref = useModalAccessibility<HTMLElement>(true, onClose);
  return (
    <section ref={ref} tabIndex={-1} role="dialog" aria-modal="true">
      <button type="button">关闭弹窗</button>
    </section>
  );
}

function RestoreHarness() {
  const [open, setOpen] = useState(false);
  return (
    <main>
      <button type="button" onClick={() => setOpen(true)}>打开弹窗</button>
      {open && <ClosingDialog onClose={() => setOpen(false)} />}
    </main>
  );
}

function NestedDialog({ onParentClose, onChildClose }: { onParentClose: () => void; onChildClose: () => void }) {
  const parentRef = useModalAccessibility<HTMLElement>(true, onParentClose);
  const childRef = useModalAccessibility<HTMLElement>(true, onChildClose);
  return (
    <section ref={parentRef} tabIndex={-1} role="dialog" aria-label="父级弹窗">
      <button type="button">父级操作</button>
      <section ref={childRef} tabIndex={-1} role="dialog" aria-label="子级弹窗">
        <button type="button">子级操作</button>
      </section>
    </section>
  );
}

describe('useModalAccessibility', () => {
  it('捕获焦点、屏蔽背景并统一响应 Escape', async () => {
    const onClose = vi.fn();
    render(<Harness onClose={onClose} />);

    const first = screen.getByRole('button', { name: '第一个' });
    const last = screen.getByRole('button', { name: '最后一个' });
    const background = screen.getByRole('button', { name: '背景操作' });
    await waitFor(() => expect(first).toHaveFocus());
    expect(background.inert).toBe(true);

    fireEvent.keyDown(document, { key: 'Tab', shiftKey: true });
    expect(last).toHaveFocus();

    last.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(first).toHaveFocus();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('关闭后将焦点还给打开弹窗的控件', async () => {
    render(<RestoreHarness />);

    const opener = screen.getByRole('button', { name: '打开弹窗' });
    opener.focus();
    fireEvent.click(opener);
    await waitFor(() => expect(screen.getByRole('button', { name: '关闭弹窗' })).toHaveFocus());

    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(opener).toHaveFocus());
  });

  it('嵌套弹窗按 Escape 时只关闭最上层', async () => {
    const onParentClose = vi.fn();
    const onChildClose = vi.fn();
    render(<NestedDialog onParentClose={onParentClose} onChildClose={onChildClose} />);

    await waitFor(() => expect(screen.getByRole('button', { name: '子级操作' })).toHaveFocus());
    fireEvent.keyDown(document, { key: 'Escape' });

    expect(onChildClose).toHaveBeenCalledOnce();
    expect(onParentClose).not.toHaveBeenCalled();
  });
});
