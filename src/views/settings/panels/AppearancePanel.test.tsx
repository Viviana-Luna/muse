import { useState } from 'react';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { AppearancePanel } from './AppearancePanel';
import type { AppearanceSettings } from '../types';

function AppearancePanelHarness({ opacity = 1 }: { opacity?: number }) {
  const [appearanceSettings, setAppearanceSettings] = useState<AppearanceSettings>({
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
