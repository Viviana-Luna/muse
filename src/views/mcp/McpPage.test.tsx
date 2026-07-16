import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  listMcpServers: vi.fn(),
  getMcpServer: vi.fn(),
  createMcpServer: vi.fn(),
  updateMcpServer: vi.fn(),
  deleteMcpServer: vi.fn(),
  testMcpServer: vi.fn(),
  refreshMcpServer: vi.fn()
}));
vi.mock('@/api', () => api);

import { McpPage } from './McpPage';

describe('McpPage', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listMcpServers.mockResolvedValue([]);
    api.createMcpServer.mockImplementation(async (draft) => ({ ...draft, revision: 'm1' }));
  });
  afterEach(cleanup);

  it('新建时二选一传输方式，保存与测试保持分离', async () => {
    render(<McpPage onSelectedNameChange={vi.fn()} notify={vi.fn()} />);
    await screen.findByText('还没有 MCP 连接');
    fireEvent.click(screen.getAllByRole('button', { name: /添加 MCP/u })[0]);
    const transport = screen.getByLabelText('传输方式');
    fireEvent.change(transport, { target: { value: 'streamable_http' } });
    expect(screen.getByLabelText('URL')).toBeInTheDocument();
    expect(screen.queryByLabelText('命令')).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'docs' } });
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'https://example.test/mcp' } });
    fireEvent.click(screen.getByRole('button', { name: /保存更改/u }));
    await waitFor(() => expect(api.createMcpServer).toHaveBeenCalledOnce());
    expect(api.testMcpServer).not.toHaveBeenCalled();
    expect(screen.queryByText(/市场|发现|推荐|提供商/u)).not.toBeInTheDocument();
  });

  it('名称的 HTML pattern 可按 Unicode Sets 规则解析', async () => {
    render(<McpPage onSelectedNameChange={vi.fn()} notify={vi.fn()} />);
    await screen.findByText('还没有 MCP 连接');
    fireEvent.click(screen.getAllByRole('button', { name: /添加 MCP/u })[0]);
    const pattern = screen.getByLabelText('名称').getAttribute('pattern');

    expect(pattern).toBe('[A-Za-z0-9_\\-]{1,64}');
    expect(() => new RegExp(pattern!, 'v')).not.toThrow();
  });

  it('名称不符合约束时在前端绑定错误且不发送保存请求', async () => {
    const notify = vi.fn();
    render(<McpPage onSelectedNameChange={vi.fn()} notify={notify} />);
    await screen.findByText('还没有 MCP 连接');
    fireEvent.click(screen.getAllByRole('button', { name: /添加 MCP/u })[0]);
    const nameInput = screen.getByLabelText('名称');
    fireEvent.change(nameInput, { target: { value: '本地测试' } });

    fireEvent.click(screen.getByRole('button', { name: /保存更改/u }));

    expect(await screen.findByRole('alert')).toHaveTextContent('仅允许 1-64 个英文字母');
    expect(nameInput).toHaveAttribute('aria-invalid', 'true');
    expect(api.createMcpServer).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'MCP 名称不符合要求', tone: 'warning' })
    );
  });
});
