import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  listSkills: vi.fn(),
  getSkill: vi.fn(),
  createSkill: vi.fn(),
  updateSkill: vi.fn(),
  deleteSkill: vi.fn()
}));
vi.mock('@/api', () => api);

import { SkillsPage } from './SkillsPage';

const skill = {
  name: 'writer',
  description: '整理写作工作流',
  content: '# 写作\n\n先整理提纲。',
  enabled: true,
  revision: 'r1',
  updated_at: '2026-07-12T10:00:00Z'
};

describe('SkillsPage', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listSkills.mockResolvedValue([skill]);
    api.getSkill.mockResolvedValue(skill);
    api.updateSkill.mockResolvedValue({ ...skill, description: '新的描述', revision: 'r2' });
  });
  afterEach(cleanup);

  it('只提供 Muse 内创建与编辑，并能切换预览后保存 revision', async () => {
    render(<SkillsPage selectedName="writer" onSelectedNameChange={vi.fn()} notify={vi.fn()} />);
    expect(await screen.findByDisplayValue('整理写作工作流')).toBeInTheDocument();
    expect(screen.queryByText(/导入|市场|安装/u)).not.toBeInTheDocument();
    fireEvent.change(screen.getByDisplayValue('整理写作工作流'), { target: { value: '新的描述' } });
    fireEvent.click(screen.getByRole('button', { name: /预览/u }));
    expect(await screen.findByRole('heading', { name: '写作' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /保存更改/u }));
    await waitFor(() => expect(api.updateSkill).toHaveBeenCalledWith('writer', expect.objectContaining({ revision: 'r1', description: '新的描述' })));
  });

  it('使用 Agent Skills 标准名称规则并在前端阻止无效保存', async () => {
    const notify = vi.fn();
    render(<SkillsPage selectedName="writer" onSelectedNameChange={vi.fn()} notify={notify} />);
    const name = await screen.findByDisplayValue('writer');
    fireEvent.change(name, { target: { value: '中文名称' } });
    fireEvent.click(screen.getByRole('button', { name: /保存更改/u }));
    expect(api.updateSkill).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledWith({
      title: 'Skill 名称无效',
      description: 'Skill 名称必须为 1-64 个小写字母、数字或单连字符组合，格式如 `git-release`。',
      tone: 'error'
    });
  });
});
