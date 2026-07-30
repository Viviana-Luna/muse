import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { PersonaActionsMenu } from './PersonaActionsMenu';

function setup() {
  const callbacks = {
    onEdit: vi.fn(),
    onCopy: vi.fn(),
    onExportFull: vi.fn(),
    onExportLight: vi.fn(),
    onDelete: vi.fn()
  };
  const loadDeletionImpact = vi.fn().mockResolvedValue({
    persona_id: 'alice',
    associated_session_count: 0,
    workspace_state_exists: false
  });
  render(
    <PersonaActionsMenu
      name="爱丽丝"
      busy={false}
      {...callbacks}
      loadDeletionImpact={loadDeletionImpact}
    />
  );
  return {
    trigger: screen.getByRole('button', { name: '爱丽丝的更多操作' }),
    callbacks,
    loadDeletionImpact
  };
}

beforeEach(() => {
  vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
    callback(0);
    return 1;
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('PersonaActionsMenu', () => {
  it('支持方向键循环、Escape 关闭并把焦点还给触发按钮', async () => {
    const { trigger } = setup();
    fireEvent.click(trigger);

    const edit = screen.getByRole('menuitem', { name: '编辑角色' });
    const copy = screen.getByRole('menuitem', { name: '创建副本' });
    const exportFull = screen.getByRole('menuitem', { name: '导出完整角色卡' });
    await waitFor(() => expect(edit).toHaveFocus());

    fireEvent.keyDown(document, { key: 'ArrowDown' });
    expect(copy).toHaveFocus();
    fireEvent.keyDown(document, { key: 'ArrowUp' });
    expect(edit).toHaveFocus();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
  });

  it('点击菜单外部会关闭弹层', () => {
    const { trigger } = setup();
    fireEvent.click(trigger);
    expect(screen.getByRole('menu')).toBeInTheDocument();

    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
  });

  it('编辑、复制和两种导出操作分别触发对应回调', () => {
    const { trigger, callbacks } = setup();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: '编辑角色' }));
    expect(callbacks.onEdit).toHaveBeenCalledOnce();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: '创建副本' }));
    expect(callbacks.onCopy).toHaveBeenCalledOnce();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: '导出完整角色卡' }));
    expect(callbacks.onExportFull).toHaveBeenCalledOnce();

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: '导出轻量角色卡' }));
    expect(callbacks.onExportLight).toHaveBeenCalledOnce();
  });

  it('确认删除后触发删除回调', async () => {
    const { trigger, callbacks } = setup();
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: '删除角色' }));

    const dialog = await screen.findByRole('alertdialog', { name: '删除角色“爱丽丝”？' });
    const confirm = screen.getByRole('button', { name: '删除角色' });
    await waitFor(() => expect(confirm).toBeEnabled());
    fireEvent.click(confirm);
    expect(callbacks.onDelete).toHaveBeenCalledOnce();
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  });

  it('删除确认前展示关联会话数量', async () => {
    const loadDeletionImpact = vi.fn().mockResolvedValue({
      persona_id: 'alice',
      associated_session_count: 3,
      workspace_state_exists: true
    });
    render(
      <PersonaActionsMenu
        name="爱丽丝"
        busy={false}
        onEdit={vi.fn()}
        onCopy={vi.fn()}
        onExportFull={vi.fn()}
        onExportLight={vi.fn()}
        onDelete={vi.fn()}
        loadDeletionImpact={loadDeletionImpact}
      />
    );
    fireEvent.click(screen.getByRole('button', { name: '爱丽丝的更多操作' }));
    fireEvent.click(screen.getByRole('menuitem', { name: '删除角色' }));

    expect(loadDeletionImpact).toHaveBeenCalledOnce();
    expect(await screen.findByText(/3 个关联会话会保留为只读历史/)).toBeVisible();
  });

  it('删除影响获取失败时禁止继续删除', async () => {
    const onDelete = vi.fn();
    const loadDeletionImpact = vi.fn().mockRejectedValue(new Error('注入影响查询失败'));
    render(
      <PersonaActionsMenu
        name="爱丽丝"
        busy={false}
        onEdit={vi.fn()}
        onCopy={vi.fn()}
        onExportFull={vi.fn()}
        onExportLight={vi.fn()}
        onDelete={onDelete}
        loadDeletionImpact={loadDeletionImpact}
      />
    );
    fireEvent.click(screen.getByRole('button', { name: '爱丽丝的更多操作' }));
    fireEvent.click(screen.getByRole('menuitem', { name: '删除角色' }));

    expect(
      await screen.findByText('无法核对删除影响，已禁止继续删除，请关闭后重试。')
    ).toBeVisible();
    const confirm = screen.getByRole('button', { name: '删除角色' });
    expect(confirm).toBeDisabled();
    fireEvent.click(confirm);
    expect(onDelete).not.toHaveBeenCalled();
  });
});
