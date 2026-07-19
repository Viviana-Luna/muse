import { renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import type { VisualPack } from '@/types';
import { resolveThemeModeFromLuminance, usePersonaTheme } from './usePersonaTheme';

const visualPack: VisualPack = {
  id: 'visual-muse',
  name: 'Muse 展示包',
  portrait_path: '/assets/portrait.webp',
  background_path: '/assets/background.webp',
  avatar_path: '/assets/avatar.webp',
  theme_color: '#d8596f',
  theme_mode: 'dark',
  layout_mode: 'portrait-right',
  portrait_frame: 'portrait',
  portrait_fit: 'cover',
  portrait_position_x: 50,
  portrait_position_y: 50,
  portrait_scale: 100,
  fallback_text: '暂无图片',
  version: '1.0.0',
  notes: ''
};

describe('resolveThemeModeFromLuminance', () => {
  it('将低亮度图片识别为暗色界面', () => {
    expect(resolveThemeModeFromLuminance(0.35)).toBe('dark');
    expect(resolveThemeModeFromLuminance(0.559)).toBe('dark');
  });

  it('将高亮度图片识别为亮色界面', () => {
    expect(resolveThemeModeFromLuminance(0.56)).toBe('light');
    expect(resolveThemeModeFromLuminance(0.82)).toBe('light');
  });

  it('没有视觉包时保持图片路径为空，不注入默认角色资产', () => {
    const { result } = renderHook(() => usePersonaTheme(null));

    expect(result.current.portraitPath).toBe('');
    expect(result.current.avatarPath).toBe('');
  });

  it('只解析头像与立绘，不再向舞台暴露旧背景资源', () => {
    const { result } = renderHook(() => usePersonaTheme(visualPack));

    expect(result.current).not.toHaveProperty('backgroundPath');
    expect(result.current.portraitPath).toBe('/assets/portrait.webp');
    expect(result.current.avatarPath).toBe('/assets/avatar.webp');
  });

  it('旧展示构图字段不再改变固定裁剪结果的舞台展示', () => {
    const { result } = renderHook(() => usePersonaTheme({
      ...visualPack,
      portrait_frame: 'wide',
      portrait_fit: 'contain',
      portrait_position_x: 10,
      portrait_position_y: 90,
      portrait_scale: 170
    }));

    expect(result.current.rootStyle).toMatchObject({
      '--portrait-frame-ratio': '3 / 4',
      '--portrait-fit': 'cover',
      '--portrait-object-x': '50%',
      '--portrait-object-y': '50%',
      '--portrait-scale': '1'
    });
  });
});
