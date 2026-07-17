import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { RuntimeModeSelector } from './RuntimeModeSelector';

describe('RuntimeModeSelector', () => {
  afterEach(cleanup);

  it('展示当前模式并允许显式切换三种模式', () => {
    const onChange = vi.fn();
    render(<RuntimeModeSelector value="daily" onChange={onChange} />);

    fireEvent.click(screen.getByRole('button', { name: '运行模式：日常' }));
    expect(screen.getByRole('menu', { name: '选择运行模式' })).toBeVisible();
    expect(screen.getAllByRole('menuitemradio')).toHaveLength(3);
    expect(screen.getByRole('menuitemradio', { name: /日常/ })).toHaveAttribute(
      'aria-checked',
      'true'
    );

    fireEvent.click(screen.getByRole('menuitemradio', { name: /工作/ }));
    expect(onChange).toHaveBeenCalledWith('focus_build');
    expect(screen.queryByRole('menu', { name: '选择运行模式' })).not.toBeInTheDocument();
  });

  it('支持 Escape 关闭菜单并恢复触发按钮焦点', async () => {
    render(<RuntimeModeSelector value="focus_plan" onChange={vi.fn()} />);
    const trigger = screen.getByRole('button', { name: '运行模式：计划' });
    fireEvent.click(trigger);
    fireEvent.keyDown(screen.getByRole('menu', { name: '选择运行模式' }), {
      key: 'Escape'
    });

    expect(screen.queryByRole('menu', { name: '选择运行模式' })).not.toBeInTheDocument();
    await new Promise((resolve) => window.requestAnimationFrame(resolve));
    expect(trigger).toHaveFocus();
  });

  it('切换中保持按钮尺寸稳定并禁止重复操作', () => {
    render(<RuntimeModeSelector value="focus_build" switching onChange={vi.fn()} />);
    const trigger = screen.getByRole('button', { name: '运行模式：工作' });
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveTextContent('工作');
  });
});
