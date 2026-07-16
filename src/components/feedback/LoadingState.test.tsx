import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { AppLoadingScreen, SectionLoading } from './LoadingState';

describe('LoadingState', () => {
  afterEach(() => cleanup());

  it('全局加载页展示 Muse 品牌和明确的忙碌语义', () => {
    const { container } = render(<AppLoadingScreen />);

    const status = screen.getByRole('status', { name: '正在加载 Muse' });
    expect(status).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByText('正在准备角色、会话与运行时状态…')).toBeInTheDocument();
    expect(container.querySelector('.app-loading-brand img')).toHaveAttribute(
      'src',
      '/assets/muse-logo.png'
    );
  });

  it('局部加载只渲染实线圆环和当前区域文案', () => {
    const { container } = render(
      <SectionLoading
        label="正在加载设置中心"
        description="正在读取本地设置…"
        variant="surface"
      />
    );

    expect(screen.getByRole('status', { name: '正在加载设置中心' })).toHaveClass(
      'is-surface'
    );
    expect(screen.getByText('正在读取本地设置…')).toBeInTheDocument();
    expect(container.querySelector('.muse-loading-spinner')).toBeInTheDocument();
    expect(container.querySelector('img')).not.toBeInTheDocument();
  });
});
