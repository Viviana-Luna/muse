import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApprovalModePicker } from './ApprovalModePicker';

describe('ApprovalModePicker', () => {
  afterEach(cleanup);

  it('展示真实三档语义并可切换 AUTO 模式', () => {
    const onChange = vi.fn();
    render(<ApprovalModePicker value="manual" onChange={onChange} />);

    fireEvent.click(screen.getByRole('button', { name: /手动审批/ }));
    const dialog = screen.getByRole('dialog', { name: '选择审批模式' });
    expect(within(dialog).getByText('手动审批')).toBeVisible();
    expect(within(dialog).getByText('AUTO 模式')).toBeVisible();
    expect(within(dialog).getByText('YOLO 模式')).toBeVisible();
    fireEvent.click(within(dialog).getByRole('button', { name: /AUTO 模式/ }));
    expect(onChange).toHaveBeenCalledWith('auto');
  });

  it('YOLO 必须经过危险确认', () => {
    const onChange = vi.fn();
    render(<ApprovalModePicker value="manual" onChange={onChange} />);

    fireEvent.click(screen.getByRole('button', { name: /手动审批/ }));
    fireEvent.click(screen.getByRole('button', { name: /YOLO 模式/ }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole('alertdialog', { name: '开启当前会话的 YOLO 模式？' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '开启 YOLO' }));
    expect(onChange).toHaveBeenCalledWith('yolo');
  });
});
