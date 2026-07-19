import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApprovalModePicker } from './ApprovalModePicker';

describe('ApprovalModePicker', () => {
  afterEach(cleanup);

  it('展示真实三档语义并可切换 AUTO 模式', () => {
    const onChange = vi.fn();
    render(<ApprovalModePicker value="manual" onChange={onChange} />);

    fireEvent.click(screen.getByRole('button', { name: /手动审批/ }));
    const menu = screen.getByRole('menu', { name: '应如何批准 Muse 操作？' });
    expect(within(menu).getByText('手动审批')).toBeVisible();
    expect(within(menu).getByText('AUTO 模式')).toBeVisible();
    expect(within(menu).getByText('YOLO 模式')).toBeVisible();
    expect(within(menu).getByRole('menuitemradio', { name: /手动审批/ })).toHaveAttribute(
      'aria-checked',
      'true'
    );
    fireEvent.click(within(menu).getByRole('menuitemradio', { name: /AUTO 模式/ }));
    expect(onChange).toHaveBeenCalledWith('auto');
  });

  it('将浮层渲染在应用主题作用域内', () => {
    render(
      <div className="app-shell">
        <ApprovalModePicker value="auto" onChange={vi.fn()} />
      </div>
    );

    fireEvent.click(screen.getByRole('button', { name: /AUTO 模式/ }));
    expect(
      screen.getByRole('menu', { name: '应如何批准 Muse 操作？' }).closest('.app-shell')
    ).not.toBeNull();
  });

  it('YOLO 必须经过危险确认', () => {
    const onChange = vi.fn();
    render(<ApprovalModePicker value="manual" onChange={onChange} />);

    fireEvent.click(screen.getByRole('button', { name: /手动审批/ }));
    fireEvent.click(screen.getByRole('menuitemradio', { name: /YOLO 模式/ }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole('alertdialog', { name: '开启当前会话的 YOLO 模式？' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '开启 YOLO' }));
    expect(onChange).toHaveBeenCalledWith('yolo');
  });
});
