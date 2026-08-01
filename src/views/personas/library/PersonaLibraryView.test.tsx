import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { useEffect } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { PersonaLibraryItem } from '@/types';
import type { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import { usePersonaState } from '@/views/personas/hooks/usePersonaState';
import type { ResourceState, StoryPersonaSnapshot } from '@/views/story/types';
import { PersonaLibraryView } from './PersonaLibraryView';

function createPersona(index: number): PersonaLibraryItem {
  return {
    id: `persona-${index}`,
    name: `角色 ${index}`,
    summary: `角色 ${index} 的简介`,
    default_visual_pack_id: `visual-${index}`,
    author: 'Muse',
    version: '1.0.0',
    visual_preview: {
      avatar_path: `/assets/persona-${index}.png`,
      portrait_path: null
    }
  };
}

function createController() {
  return {
    setPersonaImportError: vi.fn(),
    openEditor: vi.fn(),
    refreshPersonas: vi.fn().mockResolvedValue(undefined),
    handleActivate: vi.fn(),
    handleExport: vi.fn(),
    handleDelete: vi.fn()
  } as unknown as ReturnType<typeof usePersonaController>;
}

function LibraryHarness({
  personas,
  controller = createController()
}: {
  personas: PersonaLibraryItem[];
  controller?: ReturnType<typeof usePersonaController>;
}) {
  const state = usePersonaState();

  useEffect(() => {
    state.applyPersonaList(personas, personas[0]?.id ?? null);
    // 测试夹具只在传入列表变化时提交一次事实快照。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [personas]);

  const snapshot: StoryPersonaSnapshot = {
    stateRevision: 1,
    personas: {
      personas,
      active_persona_id: personas[0]?.id ?? null
    },
    activePersonaId: personas[0]?.id ?? null,
    active: null
  };

  return (
    <PersonaLibraryView
      state={state}
      controller={controller}
      resourceState={{ status: 'ready', data: snapshot }}
      busy={false}
      notify={vi.fn()}
      onOpenMemorySource={vi.fn()}
      onClose={() => undefined}
    />
  );
}

function ResourceHarness({
  resourceState,
  controller,
  onClose = () => undefined
}: {
  resourceState: ResourceState<StoryPersonaSnapshot>;
  controller: ReturnType<typeof usePersonaController>;
  onClose?: () => void;
}) {
  const state = usePersonaState();

  return (
    <PersonaLibraryView
      state={state}
      controller={controller}
      resourceState={resourceState}
      busy={false}
      notify={vi.fn()}
      onOpenMemorySource={vi.fn()}
      onClose={onClose}
    />
  );
}

afterEach(() => cleanup());

describe('PersonaLibraryView', () => {
  it('提供明确的关闭入口并返回首页', () => {
    const onClose = vi.fn();
    const controller = createController();
    render(
      <ResourceHarness
        controller={controller}
        resourceState={{ status: 'loading' }}
        onClose={onClose}
      />
    );

    fireEvent.click(screen.getByRole('button', { name: '关闭角色库，返回首页' }));
    expect(onClose).toHaveBeenCalledOnce();
  });
  it.each([0, 1, 8, 100])(
    '%i 个角色时顶部导入、创建、搜索和筛选始终可达',
    async (count) => {
      const personas = Array.from({ length: count }, (_, index) => createPersona(index + 1));
      render(<LibraryHarness personas={personas} />);

      await screen.findByText(`${count} 个本地角色`);
      const toolbar = screen.getByLabelText('角色库筛选与操作');
      expect(within(toolbar).getByRole('searchbox', { name: '搜索角色' })).toBeInTheDocument();
      expect(within(toolbar).getByRole('combobox', { name: '角色状态' })).toBeInTheDocument();
      expect(within(toolbar).getByRole('button', { name: '导入角色卡' })).toBeInTheDocument();
      expect(within(toolbar).getByRole('button', { name: '创建角色' })).toBeInTheDocument();
    }
  );

  it('单角色仍以画廊卡片呈现，不退化为横跨页面的独立版式', async () => {
    render(<LibraryHarness personas={[createPersona(1)]} />);

    const gallery = await screen.findByLabelText('本地角色列表');
    const cards = within(gallery).getAllByRole('article');
    expect(cards).toHaveLength(1);
    expect(cards[0]).toHaveAccessibleName('角色 1');
    expect(within(cards[0]).getByRole('button', { name: '编辑' })).toBeInTheDocument();
  });

  it('未启用角色无需先切换即可从更多菜单进入编辑', async () => {
    const controller = createController();
    render(
      <LibraryHarness
        personas={[createPersona(1), createPersona(2)]}
        controller={controller}
      />
    );

    const inactiveCard = await screen.findByRole('article', { name: '角色 2' });
    fireEvent.click(within(inactiveCard).getByRole('button', { name: '角色 2的更多操作' }));
    fireEvent.click(screen.getByRole('menuitem', { name: '编辑角色' }));

    expect(controller.openEditor).toHaveBeenCalledWith('edit', expect.objectContaining({ id: 'persona-2' }));
  });

  it('可以从角色更多菜单进入并退出长期记忆管理', async () => {
    render(<LibraryHarness personas={[createPersona(1)]} />);

    fireEvent.click(await screen.findByRole('button', { name: '角色 1的更多操作' }));
    fireEvent.click(screen.getByRole('menuitem', { name: '管理记忆' }));

    expect(screen.getByRole('region', { name: '角色 1的长期记忆' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '返回角色库' }));
    expect(await screen.findByRole('heading', { name: '角色库' })).toBeVisible();
  });

  it('搜索无结果时可以一键清除并恢复角色画廊', async () => {
    render(<LibraryHarness personas={[createPersona(1), createPersona(2)]} />);
    const search = await screen.findByRole('searchbox', { name: '搜索角色' });

    fireEvent.change(search, { target: { value: '完全不存在的角色' } });
    expect(await screen.findByRole('heading', { name: '没有符合条件的角色' })).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '清除筛选' }));
    await waitFor(() => expect(search).toHaveValue(''));
    expect(screen.getByLabelText('本地角色列表')).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: '没有符合条件的角色' })).not.toBeInTheDocument();
  });

  it('读取角色时展示稳定骨架并暂时禁用写操作', () => {
    const controller = createController();
    const { container } = render(
      <ResourceHarness resourceState={{ status: 'loading' }} controller={controller} />
    );

    expect(screen.getByText('正在读取本地角色…')).toBeInTheDocument();
    expect(container.querySelectorAll('.persona-gallery-skeleton .persona-gallery-card')).toHaveLength(8);
    expect(screen.getByRole('button', { name: '导入角色卡' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '创建角色' })).toBeDisabled();
  });

  it('刷新提示复用标题副文案槽位，不向滚动内容插入状态行', () => {
    const controller = createController();
    const persona = createPersona(1);
    const snapshot: StoryPersonaSnapshot = {
      stateRevision: 1,
      personas: { personas: [persona], active_persona_id: persona.id },
      activePersonaId: persona.id,
      active: null
    };
    const { container } = render(
      <ResourceHarness
        resourceState={{ status: 'refreshing', data: snapshot }}
        controller={controller}
      />
    );

    const heading = container.querySelector('.persona-library-heading');
    const scroll = container.querySelector('.persona-library-scroll');
    expect(heading).toHaveTextContent('正在同步角色状态…');
    expect(scroll).not.toHaveTextContent('正在同步角色状态…');
  });

  it('读取失败时保留明确错误和可执行的重试入口', async () => {
    const controller = createController();
    render(
      <ResourceHarness
        resourceState={{ status: 'failed', error: '角色数据暂时不可用' }}
        controller={controller}
      />
    );

    const alert = screen.getByRole('alert');
    expect(within(alert).getByRole('heading', { name: '无法读取角色库' })).toBeInTheDocument();
    expect(within(alert).getByText('角色数据暂时不可用')).toBeInTheDocument();
    fireEvent.click(within(alert).getByRole('button', { name: '重试' }));
    await waitFor(() => expect(controller.refreshPersonas).toHaveBeenCalledTimes(1));
  });

  it('真实空角色库提供创建与导入两条清晰起步路径', async () => {
    render(<LibraryHarness personas={[]} />);

    const emptyState = await screen.findByRole('heading', { name: '角色库还是空的' });
    const emptySection = emptyState.closest('section');
    expect(emptySection).not.toBeNull();
    expect(within(emptySection as HTMLElement).getByRole('button', { name: '创建角色' })).toBeInTheDocument();
    expect(within(emptySection as HTMLElement).getByRole('button', { name: '导入角色卡' })).toBeInTheDocument();
  });
});
