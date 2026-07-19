import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  clampPersonaCropSelection,
  createInitialPersonaCropSelection,
  createPersonaImageCrops,
  panPersonaCropSelection,
  PERSONA_CROP_TARGETS,
  resolvePersonaCropRect
} from './personaImageCrop';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('角色图片裁剪数学', () => {
  it('竖向原图会为立绘和头像生成固定比例的居中裁剪区', () => {
    const initial = createInitialPersonaCropSelection(810, 1440);

    expect(resolvePersonaCropRect(810, 1440, PERSONA_CROP_TARGETS.portrait, initial)).toEqual({
      x: 0,
      y: 180,
      width: 810,
      height: 1080
    });
    expect(resolvePersonaCropRect(810, 1440, PERSONA_CROP_TARGETS.avatar, initial)).toEqual({
      x: 0,
      y: 315,
      width: 810,
      height: 810
    });
  });

  it('放大时缩小原图采样区域但保持输出比例不变', () => {
    const rect = resolvePersonaCropRect(810, 1440, PERSONA_CROP_TARGETS.portrait, {
      centerX: 405,
      centerY: 720,
      zoom: 2
    });

    expect(rect).toEqual({ x: 202.5, y: 450, width: 405, height: 540 });
    expect(rect.width / rect.height).toBeCloseTo(3 / 4);
  });

  it('位置和缩放始终被限制在原图边界内', () => {
    const clamped = clampPersonaCropSelection(810, 1440, PERSONA_CROP_TARGETS.avatar, {
      centerX: -100,
      centerY: 2000,
      zoom: 9
    });
    const rect = resolvePersonaCropRect(810, 1440, PERSONA_CROP_TARGETS.avatar, clamped);

    expect(clamped.zoom).toBe(3);
    expect(rect.x).toBe(0);
    expect(rect.y + rect.height).toBe(1440);
  });

  it('拖动图片和裁剪中心按相反方向移动', () => {
    const moved = panPersonaCropSelection(
      1600,
      1200,
      PERSONA_CROP_TARGETS.portrait,
      { centerX: 800, centerY: 600, zoom: 2 },
      30,
      -20,
      300,
      400
    );

    expect(moved.centerX).toBeLessThan(800);
    expect(moved.centerY).toBeGreaterThan(600);
  });

  it('确认后生成 900×1200 立绘与 768×768 头像文件', async () => {
    const close = vi.fn();
    vi.stubGlobal('createImageBitmap', vi.fn().mockResolvedValue({
      width: 810,
      height: 1440,
      close
    }));
    const canvasSizes: Array<[number, number]> = [];
    const drawImage = vi.fn();
    const originalCreateElement = document.createElement.bind(document);
    vi.spyOn(document, 'createElement').mockImplementation((tagName, options) => {
      if (tagName !== 'canvas') return originalCreateElement(tagName, options);
      const canvas = {
        width: 0,
        height: 0,
        getContext: () => ({ drawImage }),
        toBlob(callback: BlobCallback, type?: string) {
          canvasSizes.push([this.width, this.height]);
          callback(new Blob(['cropped'], { type: type || 'image/png' }));
        }
      };
      return canvas as unknown as HTMLCanvasElement;
    });

    const initial = createInitialPersonaCropSelection(810, 1440);
    const files = await createPersonaImageCrops(
      new File(['source'], 'source.png', { type: 'image/png' }),
      { portrait: initial, avatar: initial }
    );

    expect(canvasSizes).toEqual([[900, 1200], [768, 768]]);
    expect(files.portrait).toMatchObject({ name: 'source-portrait.png', type: 'image/png' });
    expect(files.avatar).toMatchObject({ name: 'source-avatar.png', type: 'image/png' });
    expect(drawImage).toHaveBeenCalledTimes(2);
    expect(close).toHaveBeenCalledTimes(1);
  });
});
