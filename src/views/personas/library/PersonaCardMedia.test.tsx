import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const authenticatedAsset = vi.hoisted(() => ({
  useAuthenticatedAssetUrl: vi.fn((path: string) =>
    path.startsWith('/api/') ? `blob:resolved:${path}` : path
  )
}));

vi.mock('@/hooks/useAuthenticatedAsset', () => authenticatedAsset);

import { PersonaCardMedia } from './PersonaCardMedia';

beforeEach(() => {
  authenticatedAsset.useAuthenticatedAssetUrl.mockClear();
  vi.stubGlobal('IntersectionObserver', undefined);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('PersonaCardMedia', () => {
  it('头像优先于立绘，并通过认证资源 hook 解析受保护路径', async () => {
    const { container } = render(
      <PersonaCardMedia
        name="爱丽丝"
        preview={{
          avatar_path: '/api/assets/alice-avatar.png',
          portrait_path: '/assets/alice-portrait.png'
        }}
      />
    );

    await waitFor(() =>
      expect(authenticatedAsset.useAuthenticatedAssetUrl).toHaveBeenLastCalledWith(
        '/api/assets/alice-avatar.png'
      )
    );
    expect(container.querySelector('img')).toHaveAttribute(
      'src',
      'blob:resolved:/api/assets/alice-avatar.png'
    );
    expect(container.firstElementChild).toHaveAttribute('data-empty', 'false');
  });

  it('缺少头像时回退到立绘', () => {
    const { container } = render(
      <PersonaCardMedia
        name="鲍勃"
        preview={{ avatar_path: null, portrait_path: '/assets/bob-portrait.png' }}
      />
    );

    expect(authenticatedAsset.useAuthenticatedAssetUrl).toHaveBeenLastCalledWith(
      '/assets/bob-portrait.png'
    );
    expect(container.querySelector('img')).toHaveAttribute('src', '/assets/bob-portrait.png');
    expect(container.firstElementChild).toHaveAttribute('data-empty', 'false');
  });

  it('头像和立绘都为空或图片加载失败时明确展示无图状态', async () => {
    const { container, rerender } = render(
      <PersonaCardMedia
        name="无图角色"
        preview={{ avatar_path: null, portrait_path: null }}
      />
    );

    expect(container.querySelector('img')).toBeNull();
    expect(screen.getByRole('img', { name: '角色“无图角色”暂无图片' })).toBeVisible();
    expect(container.firstElementChild).toHaveAttribute('data-empty', 'true');

    rerender(
      <PersonaCardMedia
        name="损坏图片"
        preview={{ avatar_path: '/assets/broken.png', portrait_path: null }}
      />
    );
    await waitFor(() =>
      expect(container.querySelector('img')).toHaveAttribute('src', '/assets/broken.png')
    );
    fireEvent.error(container.querySelector('img')!);

    await waitFor(() =>
      expect(screen.getByRole('img', { name: '角色“损坏图片”暂无图片' })).toBeVisible()
    );
    expect(container.querySelector('img')).toBeNull();
    expect(container.firstElementChild).toHaveAttribute('data-empty', 'true');
  });
});
