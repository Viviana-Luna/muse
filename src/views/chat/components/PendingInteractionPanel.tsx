import { useState } from 'react';

import {
  ImmersiveToolApprovalCard,
  ToolApprovalCard,
  toolActionLabel
} from '@/components/tools';
import type { ToolApprovalResolver } from '@/components/tools';
import type { ChatMessage, ChatProcessStep } from '@/hooks/useRuntimeStream';
import type { RuntimeUserQuestionItem } from '@/types';

type PendingInteractionType = 'question' | 'approval';

function questionAllowsMultiple(question: RuntimeUserQuestionItem) {
  return Boolean(question.multiSelect ?? question.multi_select);
}

export interface ResolveUserQuestionFn {
  (
    requestId: string,
    answers?: Record<string, string | string[]>,
    annotations?: Record<string, { notes?: string }>
  ): void | Promise<void>;
}

export type ResolveApprovalFn = ToolApprovalResolver;

interface PendingInteractionControlsProps {
  messages: ChatMessage[];
  onResolveApproval: ResolveApprovalFn;
  onResolveUserQuestion: ResolveUserQuestionFn;
}

interface PendingStep {
  step: ChatProcessStep;
  type: PendingInteractionType;
}

interface QuestionCardProps {
  step: ChatProcessStep;
  onResolveUserQuestion: ResolveUserQuestionFn;
}

function collectPendingSteps(messages: ChatMessage[], types?: PendingInteractionType[]) {
  const pendingSteps: PendingStep[] = [];
  const allowedTypes = new Set(types ?? ['question', 'approval']);

  for (const message of messages) {
    for (const step of message.process) {
      if (allowedTypes.has('question') && step.questionRequestId && step.state === 'waiting_user') {
        pendingSteps.push({ step, type: 'question' });
      } else if (allowedTypes.has('approval') && step.approvalId && step.state === 'waiting_approval') {
        pendingSteps.push({ step, type: 'approval' });
      }
    }
  }

  return pendingSteps;
}

export function hasPendingInteractions(messages: ChatMessage[]) {
  return collectPendingSteps(messages).length > 0;
}

function useQuestionRequest(step: ChatProcessStep, onResolveUserQuestion: ResolveUserQuestionFn) {
  const questions = step.questions || [];
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [customText, setCustomText] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const canPickDirectly =
    questions.length === 1 && questions.every((question) => !questionAllowsMultiple(question));

  const toggleOption = (question: RuntimeUserQuestionItem, label: string) => {
    setSelected((value) => {
      const current = value[question.question] || [];
      if (questionAllowsMultiple(question)) {
        const next = current.includes(label)
          ? current.filter((item) => item !== label)
          : [...current, label];
        return { ...value, [question.question]: next };
      }
      return { ...value, [question.question]: [label] };
    });
  };

  const buildPayload = () => {
    const answers: Record<string, string | string[]> = {};
    for (const question of questions) {
      const labels = selected[question.question] || [];
      const custom = customText[question.question]?.trim();
      const picked = labels.filter((label) => label !== '__other__');
      if (custom) picked.push(custom);
      if (picked.length > 0) {
        answers[question.question] = questionAllowsMultiple(question) ? picked : picked[picked.length - 1];
      }
    }
    return { answers };
  };

  const allAnswered = questions.every((question) => {
    const labels = selected[question.question] || [];
    const custom = customText[question.question]?.trim();
    return labels.some((label) => label !== '__other__') || Boolean(custom);
  });

  const submit = async () => {
    if (!step.questionRequestId || !allAnswered || submitting) return;
    setSubmitting(true);
    const payload = buildPayload();
    try {
      await onResolveUserQuestion(step.questionRequestId, payload.answers);
    } finally {
      setSubmitting(false);
    }
  };

  const submitDirectChoice = async (question: RuntimeUserQuestionItem, label: string) => {
    if (!step.questionRequestId || submitting) return;
    setSubmitting(true);
    try {
      await onResolveUserQuestion(step.questionRequestId, { [question.question]: label });
    } finally {
      setSubmitting(false);
    }
  };

  const submitDirectCustom = async (question: RuntimeUserQuestionItem) => {
    const custom = customText[question.question]?.trim();
    if (!step.questionRequestId || !custom || submitting) return;
    setSubmitting(true);
    try {
      await onResolveUserQuestion(step.questionRequestId, { [question.question]: custom });
    } finally {
      setSubmitting(false);
    }
  };

  const cancel = async () => {
    if (!step.questionRequestId || submitting) return;
    setSubmitting(true);
    try {
      await onResolveUserQuestion(step.questionRequestId);
    } finally {
      setSubmitting(false);
    }
  };

  return {
    allAnswered,
    canPickDirectly,
    cancel,
    customText,
    questions,
    selected,
    setCustomText,
    submit,
    submitDirectChoice,
    submitDirectCustom,
    submitting,
    toggleOption
  };
}

function ImmersiveQuestionCard({ step, onResolveUserQuestion }: QuestionCardProps) {
  const {
    allAnswered,
    canPickDirectly,
    cancel,
    customText,
    questions,
    selected,
    setCustomText,
    submit,
    submitDirectChoice,
    submitDirectCustom,
    submitting,
    toggleOption
  } = useQuestionRequest(step, onResolveUserQuestion);
  const isPlanConfirmation = step.toolName === 'exit_plan_mode';
  const title = isPlanConfirmation ? '计划确认' : '需要你的选择';
  const waitingLabel = isPlanConfirmation ? '等待确认' : '等待回答';

  return (
    <section className="immersive-question-card galgame-choice-card pending-card glass-polish" aria-label={title}>
      <header className="galgame-choice-head">
        <span>
          <b>{title}</b>
          <em>{toolActionLabel(step.toolName)}</em>
        </span>
        <span className="tool-event-badges">
          <i>用户输入</i>
          <i>{waitingLabel}</i>
        </span>
      </header>
      <p className="approval-request-message">{step.message}</p>
      {step.interactionError && (
        <p className="pending-interaction-error" role="alert">
          {step.interactionError}
        </p>
      )}
      <div className="user-question-list">
        {questions.map((question) => {
          const labels = selected[question.question] || [];
          const multiple = questionAllowsMultiple(question);
          return (
            <section className="user-question-field" key={question.question}>
              <div className="galgame-choice-prompt">
                <span>{question.header || '选择'}</span>
                <p>{question.question}</p>
              </div>
              <div className={`user-question-options ${multiple ? 'multi' : 'single'}`}>
                {question.options.map((option) => {
                  const active = labels.includes(option.label);
                  return (
                    <button
                      type="button"
                      className={active ? 'active' : ''}
                      key={option.label}
                      disabled={submitting}
                      onClick={() =>
                        canPickDirectly
                          ? void submitDirectChoice(question, option.label)
                          : toggleOption(question, option.label)
                      }
                    >
                      <strong>{option.label}</strong>
                      <small>{option.description}</small>
                    </button>
                  );
                })}
              </div>
              <details className="user-question-custom-wrap">
                <summary>自定义回答</summary>
                <textarea
                  className="user-question-custom"
                  value={customText[question.question] || ''}
                  onChange={(event) =>
                    setCustomText((value) => ({ ...value, [question.question]: event.target.value }))
                  }
                  placeholder={multiple ? '输入自定义回答，可与上方选项一起提交' : '输入自定义回答，填写后可直接提交'}
                  rows={3}
                />
                {canPickDirectly && (
                  <button
                    type="button"
                    className="choice-submit-inline"
                    disabled={submitting || !customText[question.question]?.trim()}
                    onClick={() => void submitDirectCustom(question)}
                  >
                    提交自定义回答
                  </button>
                )}
              </details>
            </section>
          );
        })}
      </div>
      <span className={`approval-actions standalone ${canPickDirectly ? 'choice-cancel-only' : ''}`}>
        {!canPickDirectly && (
          <button type="button" className="primary" disabled={!allAnswered || submitting} onClick={() => void submit()}>
            提交选择
          </button>
        )}
        <button type="button" disabled={submitting} onClick={() => void cancel()}>
          取消回答
        </button>
      </span>
    </section>
  );
}

function ClassicQuestionCard({ step, onResolveUserQuestion }: QuestionCardProps) {
  const {
    allAnswered,
    canPickDirectly,
    cancel,
    customText,
    questions,
    selected,
    setCustomText,
    submit,
    submitDirectChoice,
    submitDirectCustom,
    submitting,
    toggleOption
  } = useQuestionRequest(step, onResolveUserQuestion);

  return (
    <article className="classic-interaction-card classic-question-card" aria-label="等待用户输入">
      <header className="classic-interaction-card-head">
        <span>
          <b>需要输入</b>
          <em>{toolActionLabel(step.toolName)}</em>
        </span>
        <i>等待回答</i>
      </header>
      <p>{step.message}</p>
      {step.interactionError && (
        <p className="pending-interaction-error" role="alert">
          {step.interactionError}
        </p>
      )}
      {questions.map((question) => {
        const labels = selected[question.question] || [];
        const multiple = questionAllowsMultiple(question);
        return (
          <fieldset className="classic-question-field" key={question.question}>
            <legend>
              <span>{question.header || '选择'}</span>
              {question.question}
            </legend>
            <div className={`classic-choice-options ${multiple ? 'multi' : 'single'}`}>
              {question.options.map((option) => {
                const active = labels.includes(option.label);
                return (
                  <button
                    type="button"
                    className={active ? 'active' : ''}
                    disabled={submitting}
                    key={option.label}
                    onClick={() =>
                      canPickDirectly
                        ? void submitDirectChoice(question, option.label)
                        : toggleOption(question, option.label)
                    }
                  >
                    <strong>{option.label}</strong>
                    <small>{option.description}</small>
                  </button>
                );
              })}
            </div>
            <details className="classic-custom-answer">
              <summary>自定义回答</summary>
              <textarea
                value={customText[question.question] || ''}
                onChange={(event) =>
                  setCustomText((value) => ({ ...value, [question.question]: event.target.value }))
                }
                placeholder="输入自定义回答"
                rows={2}
              />
              {canPickDirectly && (
                <button
                  type="button"
                  disabled={submitting || !customText[question.question]?.trim()}
                  onClick={() => void submitDirectCustom(question)}
                >
                  提交自定义
                </button>
              )}
            </details>
          </fieldset>
        );
      })}
      <footer className={`classic-interaction-actions ${canPickDirectly ? 'cancel-only' : ''}`}>
        {!canPickDirectly && (
          <button type="button" className="primary" disabled={!allAnswered || submitting} onClick={() => void submit()}>
            提交
          </button>
        )}
        <button type="button" disabled={submitting} onClick={() => void cancel()}>
          取消
        </button>
      </footer>
    </article>
  );
}

export function ImmersiveInteractionLayer({
  messages,
  onResolveApproval,
  onResolveUserQuestion
}: PendingInteractionControlsProps) {
  const approvalSteps = collectPendingSteps(messages, ['approval']);
  const questionSteps = collectPendingSteps(messages, ['question']);

  return (
    <>
      {approvalSteps.length > 0 && (
        <aside className="runtime-pending-popover immersive-approval-layer" aria-label="工具执行确认">
          <div className="pending-interaction-panel">
            {approvalSteps.map(({ step }) => (
              <ImmersiveToolApprovalCard
                key={`immersive-a-${step.id}`}
                step={step}
                onResolveApproval={onResolveApproval}
              />
            ))}
          </div>
        </aside>
      )}
      {questionSteps.length > 0 && (
        <aside className="runtime-choice-popover immersive-choice-layer" aria-label="需要你的选择">
          <div className="pending-interaction-panel">
            {questionSteps.map(({ step }) => (
              <ImmersiveQuestionCard
                key={`immersive-q-${step.id}`}
                step={step}
                onResolveUserQuestion={onResolveUserQuestion}
              />
            ))}
          </div>
        </aside>
      )}
    </>
  );
}

export function ClassicInteractionPanel({
  messages,
  onResolveApproval,
  onResolveUserQuestion
}: PendingInteractionControlsProps) {
  const pendingSteps = collectPendingSteps(messages);

  if (pendingSteps.length === 0) return null;
  const isSingleApproval = pendingSteps.length === 1 && pendingSteps[0].type === 'approval';

  return (
    <section className={`classic-interaction-panel ${isSingleApproval ? 'single-approval' : ''}`} aria-label="等待用户操作">
      {!isSingleApproval && (
        <header className="classic-interaction-head">
          <span>
            <strong>等待你的决定</strong>
            <small>{pendingSteps.length} 项行动等待确认</small>
          </span>
        </header>
      )}
      <div className="classic-interaction-list">
        {pendingSteps.map(({ step, type }) =>
          type === 'question' ? (
            <ClassicQuestionCard
              key={`classic-q-${step.id}`}
              step={step}
              onResolveUserQuestion={onResolveUserQuestion}
            />
          ) : (
            <ToolApprovalCard
              key={`classic-a-${step.id}`}
              step={step}
              onResolveApproval={onResolveApproval}
            />
          )
        )}
      </div>
    </section>
  );
}
