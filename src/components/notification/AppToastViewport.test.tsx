import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { AppToastViewport } from './AppToastViewport';

describe('AppToastViewport', () => {
  it('为亮色主题通知附加明确的主题类名', () => {
    render(
      <AppToastViewport
        themeMode="light"
        toasts={[
          {
            id: 1,
            title: '余额检测失败',
            description: 'API Key 无效',
            tone: 'error'
          }
        ]}
        onDismiss={vi.fn()}
      />
    );

    expect(screen.getByText('余额检测失败').closest('.app-toast')).toHaveClass(
      'theme-light',
      'error'
    );
  });
});
