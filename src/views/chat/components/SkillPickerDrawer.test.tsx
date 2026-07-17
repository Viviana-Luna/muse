import { fireEvent, render, screen } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { describe, expect, it, vi } from 'vitest';

import type { RuntimeSkillSummary } from '@/types';
import { SkillPickerDrawer } from './SkillPickerDrawer';

const SKILLS: RuntimeSkillSummary[] = [
  {
    name: 'skill-creator',
    description: '创建和维护高质量 Skill',
    revision: 'creator-revision',
    source: 'builtin'
  },
  {
    name: 'pdf',
    description: '读取和生成 PDF 文档',
    revision: 'pdf-revision',
    source: 'user_store'
  }
];

function renderDrawer(overrides: Partial<ComponentProps<typeof SkillPickerDrawer>> = {}) {
  const props: ComponentProps<typeof SkillPickerDrawer> = {
    open: true,
    skills: SKILLS,
    loading: false,
    error: null,
    omittedSkillCount: 0,
    onSelect: vi.fn(),
    onClose: vi.fn(),
    onRetry: vi.fn(),
    ...overrides
  };
  render(<SkillPickerDrawer {...props} />);
  return props;
}

describe('SkillPickerDrawer', () => {
  it('支持搜索并把选择结果交回输入区', () => {
    const props = renderDrawer();

    expect(screen.getByRole('dialog', { name: '选择 Skill' })).toBeVisible();
    fireEvent.change(screen.getByPlaceholderText('搜索名称或用途'), {
      target: { value: 'creator' }
    });
    expect(screen.getByRole('button', { name: /skill-creator/u })).toBeVisible();
    expect(screen.queryByRole('button', { name: /pdf/u })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: /skill-creator/u }));
    expect(props.onSelect).toHaveBeenCalledWith(SKILLS[0]);
  });

  it('支持 Escape 关闭和加载失败重试', () => {
    const onClose = vi.fn();
    const onRetry = vi.fn();
    renderDrawer({ skills: [], error: '运行时目录暂不可用', onClose, onRetry });

    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    expect(onRetry).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
