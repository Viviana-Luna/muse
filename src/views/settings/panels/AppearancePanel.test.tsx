import { useState } from 'react';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { AppearancePanel } from './AppearancePanel';
import type { AppearanceSettings } from '../types';

function AppearancePanelHarness({
  opacity = 1,
  theme = 'system',
  backgroundTheme = 'dark'
}: {
  opacity?: number;
  theme?: AppearanceSettings['theme'];
  backgroundTheme?: AppearanceSettings['backgroundTheme'];
}) {
  const [appearanceSettings, setAppearanceSettings] = useState<AppearanceSettings>({
    theme,
    backgroundTheme,
    backgroundBlur: 18,
    backgroundOpacity: opacity,
    motionLevel: 'full'
  });

  return (
    <AppearancePanel
      appearanceSettings={appearanceSettings}
      setAppearanceSettings={setAppearanceSettings}
    />
  );
}

describe('AppearancePanel', () => {
  afterEach(cleanup);

  it('提供跟随系统、深色和浅色主题并立即切换选中状态', () => {
    render(<AppearancePanelHarness />);

    const system = screen.getByRole('button', { name: /跟随系统/ });
    const dark = screen.getByRole('button', { name: /深色主题/ });
    const light = screen.getByRole('button', { name: /浅色主题/ });
    expect(system).toHaveAttribute('aria-pressed', 'true');
    expect(dark).toHaveAttribute('aria-pressed', 'false');
    expect(light).toHaveAttribute('aria-pressed', 'false');

    fireEvent.click(light);

    expect(system).toHaveAttribute('aria-pressed', 'false');
    expect(light).toHaveAttribute('aria-pressed', 'true');
  });

  it('背景明暗与界面主题分别切换', () => {
    render(<AppearancePanelHarness theme="dark" />);

    const interfaceDark = screen.getByRole('button', { name: /深色主题/ });
    const backgroundDark = screen.getByRole('button', { name: /深色背景/ });
    const backgroundLight = screen.getByRole('button', { name: /浅色背景/ });
    expect(interfaceDark).toHaveAttribute('aria-pressed', 'true');
    expect(backgroundDark).toHaveAttribute('aria-pressed', 'true');

    fireEvent.click(backgroundLight);

    expect(interfaceDark).toHaveAttribute('aria-pressed', 'true');
    expect(backgroundDark).toHaveAttribute('aria-pressed', 'false');
    expect(backgroundLight).toHaveAttribute('aria-pressed', 'true');
  });

  it('默认保持实色背景并可切换到 Windows 半透明背景', () => {
    render(<AppearancePanelHarness />);

    const solid = screen.getByRole('button', { name: /实色背景/ });
    const translucent = screen.getByRole('button', { name: /半透明背景/ });
    expect(solid).toHaveAttribute('aria-pressed', 'true');
    expect(translucent).toHaveAttribute('aria-pressed', 'false');

    fireEvent.click(translucent);

    expect(solid).toHaveAttribute('aria-pressed', 'false');
    expect(translucent).toHaveAttribute('aria-pressed', 'true');
  });

  it('把已有的非 1 可见度归入半透明模式并允许恢复实色', () => {
    render(<AppearancePanelHarness opacity={0.8} />);

    const solid = screen.getByRole('button', { name: /实色背景/ });
    const translucent = screen.getByRole('button', { name: /半透明背景/ });
    expect(translucent).toHaveAttribute('aria-pressed', 'true');

    fireEvent.click(solid);

    expect(solid).toHaveAttribute('aria-pressed', 'true');
    expect(translucent).toHaveAttribute('aria-pressed', 'false');
  });
});
