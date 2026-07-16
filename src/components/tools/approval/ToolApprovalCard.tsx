import { Fragment, useState } from 'react';

import type { ChatProcessStep } from '@/hooks/useRuntimeStream';

import { toolActionLabel, toolPresentation } from '../toolPresentation';

export interface ToolApprovalResolver {
  (approvalId: string, approved: boolean): void | Promise<void>;
}

export interface ToolApprovalCardProps {
  step: ChatProcessStep;
  onResolveApproval: ToolApprovalResolver;
}

function ToolSummary({ step }: Pick<ToolApprovalCardProps, 'step'>) {
  return (
    <>
      {step.interactionError && (
        <p className="pending-interaction-error" role="alert">
          {step.interactionError}
        </p>
      )}
      {step.approvalHint && <p className="tool-event-hint">{step.approvalHint}</p>}
      {step.toolSummary && (
        <dl className="tool-event-summary">
          {step.toolSummary.map((row) => (
            <Fragment key={`${step.id}-summary-${row.label}`}>
              <dt>{row.label}</dt>
              <dd>{row.value}</dd>
            </Fragment>
          ))}
        </dl>
      )}
      {step.argumentsPreview && (
        <details className="tool-event-raw">
          <summary>查看原始参数</summary>
          <code className="process-arguments">{step.argumentsPreview}</code>
        </details>
      )}
    </>
  );
}

export function ImmersiveToolApprovalCard({ step, onResolveApproval }: ToolApprovalCardProps) {
  const [submitting, setSubmitting] = useState(false);

  const resolve = async (approved: boolean) => {
    if (!step.approvalId || submitting) return;
    setSubmitting(true);
    try {
      await onResolveApproval(step.approvalId, approved);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <section className={`immersive-approval-card pending-card glass-polish ${step.riskTone || 'warn'}`} aria-label="工具执行确认">
      <header className="approval-request-head">
        <span>
          <b>请求批准</b>
          <em>{toolActionLabel(step.toolName)}</em>
        </span>
        <span className="tool-event-badges">
          <i>{step.riskLabel || step.risk || '未知风险'}</i>
          <i>等待确认</i>
        </span>
      </header>
      <p className="approval-request-message">{step.message}</p>
      <ToolSummary step={step} />
      <span className="approval-actions standalone">
        <button type="button" className="primary" disabled={submitting} onClick={() => void resolve(true)}>
          {submitting ? '正在提交…' : '允许'}
        </button>
        <button type="button" disabled={submitting} onClick={() => void resolve(false)}>
          {submitting ? '正在提交…' : '取消本次执行'}
        </button>
      </span>
    </section>
  );
}

export function ToolApprovalCard({ step, onResolveApproval }: ToolApprovalCardProps) {
  const [submitting, setSubmitting] = useState(false);
  const presentation = toolPresentation(step.toolName);
  const ActionIcon = presentation.icon;

  const resolve = async (approved: boolean) => {
    if (!step.approvalId || submitting) return;
    setSubmitting(true);
    try {
      await onResolveApproval(step.approvalId, approved);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <article
      className={`classic-interaction-card classic-approval-card approval-decision-card ${step.riskTone || 'warn'}`}
      aria-label="行动确认"
    >
      <header className="approval-decision-head">
        <span className="approval-action-icon" aria-hidden="true">
          <ActionIcon />
        </span>
        <span className="approval-decision-title">
          <b>{toolActionLabel(step.toolName)}</b>
          <small>{presentation.shortImpact}</small>
        </span>
        <i className={`approval-risk-badge ${step.riskTone || 'warn'}`}>
          {step.riskLabel || step.risk || '需确认'}
        </i>
      </header>
      {step.toolSummary && (
        <dl className="approval-compact-summary" aria-label="本次行动范围">
          {step.toolSummary.map((row) => (
            <Fragment key={`${step.id}-approval-${row.label}`}>
              <dt>{row.label}</dt>
              <dd>{row.value}</dd>
            </Fragment>
          ))}
        </dl>
      )}
      {step.interactionError && (
        <p className="pending-interaction-error" role="alert">
          {step.interactionError}
        </p>
      )}
      <footer className="classic-interaction-actions approval-decision-actions">
        <button type="button" className="primary" disabled={submitting} onClick={() => void resolve(true)}>
          {submitting ? '正在提交…' : '允许并继续'}
        </button>
        <button type="button" disabled={submitting} onClick={() => void resolve(false)}>
          {submitting ? '正在提交…' : '拒绝本次行动'}
        </button>
      </footer>
    </article>
  );
}
