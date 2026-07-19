import { useEffect, useMemo, useState } from 'react';
import type { CSSProperties } from 'react';

import type { VisualPack, VisualThemeMode } from '@/types';
import { resolveAuthenticatedAssetUrl, useAuthenticatedAssetUrl } from '@/hooks/useAuthenticatedAsset';

const DEFAULT_THEME_COLOR = '#d8596f';

export type ResolvedVisualThemeMode = Exclude<VisualThemeMode, 'auto'>;

export interface DetectedImageTheme {
  themeColor: string;
  themeMode: ResolvedVisualThemeMode;
  averageLuminance: number;
}

export function resolveThemeModeFromLuminance(averageLuminance: number): ResolvedVisualThemeMode {
  return averageLuminance >= 0.56 ? 'light' : 'dark';
}

type PersonaThemeStyle = CSSProperties & {
  '--theme-color': string;
  '--theme-color-hover': string;
  '--theme-color-glow': string;
  '--theme-color-bg': string;
  '--portrait-frame-ratio': string;
  '--portrait-fit': string;
  '--portrait-object-x': string;
  '--portrait-object-y': string;
  '--portrait-scale': string;
};

function normalizeVisualAsset(path: string | undefined): string {
  return path?.trim() ?? '';
}

function normalizeThemeColor(color: string | undefined): string {
  const value = color?.trim();
  if (!value) return DEFAULT_THEME_COLOR;
  return value;
}

function normalizeThemeMode(mode: string | undefined): VisualThemeMode {
  if (mode === 'dark' || mode === 'light') return mode;
  return 'auto';
}

function hexToRgb(color: string): [number, number, number] | null {
  const value = color.trim().replace(/^#/, '');
  if (/^[0-9a-fA-F]{3}$/.test(value)) {
    return value.split('').map((item) => Number.parseInt(item + item, 16)) as [number, number, number];
  }
  if (/^[0-9a-fA-F]{6}$/.test(value)) {
    return [
      Number.parseInt(value.slice(0, 2), 16),
      Number.parseInt(value.slice(2, 4), 16),
      Number.parseInt(value.slice(4, 6), 16)
    ];
  }
  return null;
}

function colorWithAlpha(color: string, alpha: number): string {
  const rgb = hexToRgb(color);
  if (!rgb) return `color-mix(in srgb, ${color}, transparent ${Math.round((1 - alpha) * 100)}%)`;
  return `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${alpha})`;
}

function rgbToHex(red: number, green: number, blue: number): string {
  return `#${[red, green, blue]
    .map((value) => Math.round(value).toString(16).padStart(2, '0'))
    .join('')}`;
}

async function detectImageThemeFromUrl(src: string): Promise<DetectedImageTheme> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.crossOrigin = 'anonymous';
    image.onload = () => {
      const canvas = document.createElement('canvas');
      const size = 48;
      canvas.width = size;
      canvas.height = size;
      const context = canvas.getContext('2d', { willReadFrequently: true });
      if (!context) {
        reject(new Error('当前浏览器无法读取图片颜色。'));
        return;
      }

      context.drawImage(image, 0, 0, size, size);
      const pixels = context.getImageData(0, 0, size, size).data;
      let best: [number, number, number] | null = null;
      let bestScore = -1;
      let luminanceTotal = 0;
      let redTotal = 0;
      let greenTotal = 0;
      let blueTotal = 0;
      let sampleCount = 0;

      for (let index = 0; index < pixels.length; index += 4) {
        const red = pixels[index];
        const green = pixels[index + 1];
        const blue = pixels[index + 2];
        const alpha = pixels[index + 3];
        if (alpha < 180) continue;

        const max = Math.max(red, green, blue);
        const min = Math.min(red, green, blue);
        const saturation = max === 0 ? 0 : (max - min) / max;
        const luminance = (red * 0.2126 + green * 0.7152 + blue * 0.0722) / 255;
        luminanceTotal += luminance;
        redTotal += red;
        greenTotal += green;
        blueTotal += blue;
        sampleCount += 1;

        if (luminance < 0.14 || luminance > 0.92 || saturation < 0.1) continue;
        const score = saturation * 1.4 + (1 - Math.abs(luminance - 0.58));
        if (score > bestScore) {
          bestScore = score;
          best = [red, green, blue];
        }
      }

      if (sampleCount === 0) {
        reject(new Error('图片中没有可用于识别的像素。'));
        return;
      }

      const averageLuminance = luminanceTotal / sampleCount;
      const fallback: [number, number, number] = [
        redTotal / sampleCount,
        greenTotal / sampleCount,
        blueTotal / sampleCount
      ];
      const theme = best ?? fallback;
      resolve({
        themeColor: rgbToHex(theme[0], theme[1], theme[2]),
        themeMode: resolveThemeModeFromLuminance(averageLuminance),
        averageLuminance
      });
    };
    image.onerror = () => reject(new Error('图片加载失败，无法识别主题。'));
    image.src = src;
  });
}

export async function detectImageTheme(src: string): Promise<DetectedImageTheme> {
  const resolved = await resolveAuthenticatedAssetUrl(src);
  try {
    return await detectImageThemeFromUrl(resolved.url);
  } finally {
    resolved.revoke();
  }
}

function resolveStageStateClass(voiceStatus?: string): string {
  if (voiceStatus === '正在语音播报') return 'is-speaking';
  if (voiceStatus === '正在合成语音') return 'is-synthesizing';
  if (voiceStatus === '正在听你说话') return 'is-listening';
  return 'is-idle';
}

export function usePersonaTheme(
  visualPack: VisualPack | null,
  voiceStatus?: string
) {
  const configuredPortraitPath = normalizeVisualAsset(visualPack?.portrait_path);
  const configuredAvatarPath = normalizeVisualAsset(visualPack?.avatar_path);
  const resolvedPortraitPath = useAuthenticatedAssetUrl(configuredPortraitPath);
  const resolvedAvatarPath = useAuthenticatedAssetUrl(configuredAvatarPath);
  const configuredThemeMode = normalizeThemeMode(visualPack?.theme_mode);
  const [detectedThemeMode, setDetectedThemeMode] = useState<ResolvedVisualThemeMode>('dark');

  useEffect(() => {
    if (configuredThemeMode !== 'auto') return;
    if (!resolvedPortraitPath) {
      setDetectedThemeMode('dark');
      return;
    }
    let cancelled = false;
    void detectImageTheme(resolvedPortraitPath)
      .then((detected) => {
        if (!cancelled) setDetectedThemeMode(detected.themeMode);
      })
      .catch(() => {
        if (!cancelled) setDetectedThemeMode('dark');
      });
    return () => {
      cancelled = true;
    };
  }, [configuredThemeMode, resolvedPortraitPath]);

  const resolvedThemeMode = configuredThemeMode === 'auto' ? detectedThemeMode : configuredThemeMode;
  const theme = useMemo(() => {
    const themeColor = normalizeThemeColor(visualPack?.theme_color);
    return {
      themeColor,
      themeMode: resolvedThemeMode,
      portraitPath: resolvedPortraitPath,
      avatarPath: resolvedAvatarPath,
      stageStateClass: resolveStageStateClass(voiceStatus),
      rootStyle: {
        '--theme-color': themeColor,
        '--theme-color-hover': colorWithAlpha(themeColor, 0.88),
        '--theme-color-glow': colorWithAlpha(themeColor, 0.25),
        '--theme-color-bg': colorWithAlpha(themeColor, 0.12),
        '--portrait-frame-ratio': '3 / 4',
        '--portrait-fit': 'cover',
        '--portrait-object-x': '50%',
        '--portrait-object-y': '50%',
        '--portrait-scale': '1'
      } as PersonaThemeStyle
    };
  }, [resolvedAvatarPath, resolvedPortraitPath, resolvedThemeMode, visualPack, voiceStatus]);

  return theme;
}
