import { StrictMode } from 'react';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { PersonaImageCropDialog } from './PersonaImageCropDialog';

beforeEach(() => {
  Object.defineProperty(URL, 'createObjectURL', {
    configurable: true,
    value: vi.fn(() => 'blob:persona-source')
  });
  Object.defineProperty(URL, 'revokeObjectURL', {
    configurable: true,
    value: vi.fn()
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function loadSourceImage() {
  const image = document.querySelector<HTMLImageElement>('.persona-crop-frame img');
  expect(image).not.toBeNull();
  Object.defineProperty(image, 'naturalWidth', { configurable: true, value: 810 });
  Object.defineProperty(image, 'naturalHeight', { configurable: true, value: 1440 });
  fireEvent.load(image!);
}

describe('PersonaImageCropDialog', () => {
  it('React StrictMode 下为 JPG 重新创建未撤销的预览地址', () => {
    const createObjectUrl = vi.mocked(URL.createObjectURL);
    createObjectUrl
      .mockReset()
      .mockReturnValueOnce('blob:jpg-first')
      .mockReturnValueOnce('blob:jpg-second');
    const revokeObjectUrl = vi.mocked(URL.revokeObjectURL);
    const view = render(
      <StrictMode>
        <PersonaImageCropDialog
          file={new File(['jpeg-source'], 'source.jpg', { type: 'image/jpeg' })}
          busy={false}
          onCancel={vi.fn()}
          onConfirm={vi.fn().mockResolvedValue(true)}
        />
      </StrictMode>
    );

    const image = document.querySelector<HTMLImageElement>('.persona-crop-frame img');
    expect(createObjectUrl).toHaveBeenCalledTimes(2);
    expect(image).toHaveAttribute('src', 'blob:jpg-second');
    expect(revokeObjectUrl).toHaveBeenCalledWith('blob:jpg-first');
    expect(revokeObjectUrl).not.toHaveBeenCalledWith('blob:jpg-second');
    loadSourceImage();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();

    view.unmount();
    expect(revokeObjectUrl).toHaveBeenCalledWith('blob:jpg-second');
  });

  it('提供固定立绘与头像目标，并为两个目标保留独立缩放值', () => {
    render(
      <PersonaImageCropDialog
        file={new File(['source'], 'source.png', { type: 'image/png' })}
        busy={false}
        onCancel={vi.fn()}
        onConfirm={vi.fn().mockResolvedValue(true)}
      />
    );
    loadSourceImage();

    expect(screen.getByRole('tab', { name: /立绘/ })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('900 × 1200')).toBeInTheDocument();
    expect(screen.getByText('768 × 768')).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText('缩放'), { target: { value: '1.5' } });
    expect(screen.getByText('150%')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('tab', { name: /头像/ }));
    expect(screen.getByText('100%')).toBeInTheDocument();
  });

  it('支持键盘移动、重置和取消，不会在取消时生成图片', () => {
    const onCancel = vi.fn();
    const onConfirm = vi.fn().mockResolvedValue(true);
    render(
      <PersonaImageCropDialog
        file={new File(['source'], 'source.png', { type: 'image/png' })}
        busy={false}
        onCancel={onCancel}
        onConfirm={onConfirm}
      />
    );
    loadSourceImage();

    const cropArea = screen.getByRole('application', { name: /立绘裁剪区域/ });
    fireEvent.keyDown(cropArea, { key: 'ArrowDown' });
    fireEvent.click(screen.getByRole('button', { name: '重置当前裁剪' }));
    fireEvent.click(screen.getByRole('button', { name: '取消' }));

    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });
});
