import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { PlanModeToggle } from './PlanModeToggle';

describe('PlanModeToggle', () => {
  afterEach(cleanup);

  it('默认工作态只展示进入计划入口', () => {
    const onChange = vi.fn();
    render(<PlanModeToggle value="focus_build" onChange={onChange} />);

    expect(screen.queryByText('日常')).not.toBeInTheDocument();
    expect(screen.queryByText('工作')).not.toBeInTheDocument();
    const trigger = screen.getByRole('button', { name: '进入计划模式' });
    expect(trigger).toHaveAttribute('aria-pressed', 'false');
    fireEvent.click(trigger);
    expect(onChange).toHaveBeenCalledWith('focus_plan');
  });

  it('计划态提供明确退出动作和选中状态', () => {
    const onChange = vi.fn();
    render(<PlanModeToggle value="focus_plan" onChange={onChange} />);

    const trigger = screen.getByRole('button', { name: '退出计划模式' });
    expect(trigger).toHaveTextContent('计划中');
    expect(trigger).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(trigger);
    expect(onChange).toHaveBeenCalledWith('focus_build');
  });

  it('切换中保持按钮文案稳定并禁止重复操作', () => {
    render(<PlanModeToggle value="focus_build" switching onChange={vi.fn()} />);

    const trigger = screen.getByRole('button', { name: '进入计划模式' });
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveTextContent('计划');
  });
});
