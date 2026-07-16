import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppRail } from './AppRail';

afterEach(cleanup);

describe('AppRail', () => {
  it('不向用户暴露运行时工具目录', () => {
    render(<AppRail active="chat" onNavigate={vi.fn()} />);

    expect(screen.queryByRole('button', { name: '工具' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Skill' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'MCP' })).toBeVisible();
  });
});
