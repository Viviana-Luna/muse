import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ChatProcessStep } from '@/hooks/useRuntimeStream';

import { ToolApprovalCard } from './ToolApprovalCard';

const approvalStep: ChatProcessStep = {
  id: 'approval-step-1',
  phase: 'approval_pending',
  message: '这个工具需要你的确认后才能继续。',
  state: 'waiting_approval',
  time: '10:30:00',
  approvalId: 'approval-1',
  toolName: 'web_search',
  risk: 'network',
  riskLabel: '联网',
  riskTone: 'warn',
  toolSummary: [{ label: '搜索词', value: '最新台风消息 2025年7月' }],
  argumentsPreview: '{"query":"最新台风消息 2025年7月"}'
};

describe('ToolApprovalCard', () => {
  afterEach(() => {
    cleanup();
  });

  it('把工具批准呈现为行动、影响和范围明确的确认卡', () => {
    const onResolveApproval = vi.fn();
    render(<ToolApprovalCard step={approvalStep} onResolveApproval={onResolveApproval} />);

    expect(screen.getByText('联网搜索')).toBeVisible();
    expect(screen.getByText('搜索词')).toBeVisible();
    expect(screen.queryByText('为什么需要确认和技术详情')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '允许并继续' }));
    expect(onResolveApproval).toHaveBeenCalledWith('approval-1', true);
  });

  it('明确地把拒绝动作传递给运行时', () => {
    const onResolveApproval = vi.fn();
    render(<ToolApprovalCard step={approvalStep} onResolveApproval={onResolveApproval} />);

    fireEvent.click(screen.getByRole('button', { name: '拒绝本次行动' }));
    expect(onResolveApproval).toHaveBeenCalledWith('approval-1', false);
  });

  it('审批提交失败后在原卡片内展示错误', () => {
    render(
      <ToolApprovalCard
        step={{ ...approvalStep, interactionError: '提交失败：运行时暂不可用' }}
        onResolveApproval={vi.fn()}
      />
    );

    expect(screen.getByRole('alert')).toHaveTextContent('提交失败：运行时暂不可用');
    expect(screen.getByRole('button', { name: '允许并继续' })).toBeEnabled();
  });
});
