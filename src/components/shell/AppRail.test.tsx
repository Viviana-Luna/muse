import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppRail } from './AppRail';

afterEach(cleanup);

describe('AppRail', () => {
  it('只保留有效的产品导航入口', () => {
    render(<AppRail active="chat" onNavigate={vi.fn()} />);

    expect(screen.queryByRole('button', { name: '工具' })).not.toBeInTheDocument();
    expect(screen.queryByTitle('本地 Agent 工作台')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Skill' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'MCP' })).toBeVisible();
  });
});
