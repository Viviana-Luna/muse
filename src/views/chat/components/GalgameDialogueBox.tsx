import { lazy, memo, Suspense, useMemo } from 'react';
import type { RefObject } from 'react';
import { BookHeart, ChevronDown } from 'lucide-react';

import type { ChatMessage } from '@/hooks/useRuntimeStream';
import type { Role } from '@/types';
import { useVirtualMessageWindow } from '@/views/chat/hooks/useVirtualMessageWindow';

const MarkdownMessage = lazy(() =>
  import('./MarkdownMessage').then((module) => ({ default: module.MarkdownMessage }))
);

interface GalgameDialogueBoxProps {
  messages: ChatMessage[];
  activePersonaName?: string;
  activePersonaScenario?: string;
  emptyText?: string;
  onUsePrompt?: (prompt: string) => void;
  scrollContainerRef?: RefObject<HTMLElement | null>;
}

function roleLabel(role: Role, activePersonaName?: string): string {
  if (role === 'user') return '我';
  if (role === 'assistant') return activePersonaName || '当前角色';
  return '环境';
}

function visibleDialogue(messages: ChatMessage[]): ChatMessage[] {
  return messages
    .filter((message) => message.content.trim() || message.streaming)
    // 系统提示词属于运行时契约，不能作为剧情内容泄漏给用户。
    .filter((message) => message.role !== 'system');
}

function actionLabel(message: ChatMessage) {
  const lastStep = message.process.at(-1);
  if (!lastStep) return null;

  const labels: Record<string, string> = {
    queued: '已送达',
    thinking: '正在思考',
    brief: '正在整理',
    generating: '正在回应',
    tool_running: '正在行动',
    tool_completed: '行动已完成',
    approval_pending: '等待你的许可',
    user_question_pending: '等待你的选择',
    speech_started: '正在朗读',
    speech_finished: '朗读完成',
    synthesizing: '正在组织语言',
    completed: '已完成',
    cancelled: '已停止',
    failed: '行动未完成'
  };

  return lastStep.message?.trim() || labels[lastStep.phase] || null;
}

function memoryReferenceLabel(category?: string, importance?: string) {
  const categories: Record<string, string> = {
    user_fact: '用户事实',
    user_preference: '用户偏好',
    shared_experience: '共同经历',
    commitment: '约定',
    story_state: '故事状态'
  };
  const importanceLabels: Record<string, string> = { low: '低', normal: '普通', high: '高' };
  return [
    category ? categories[category] || category : '',
    importance ? `${importanceLabels[importance] || importance}重要度` : ''
  ].filter(Boolean).join(' · ');
}

function MemoryActivityFeed({ message }: { message: ChatMessage }) {
  const activities = message.process.flatMap((step) => step.memoryActivity ? [step.memoryActivity] : []);
  if (activities.length === 0) return null;
  return (
    <aside className="story-memory-activity" aria-label="本轮长期记忆活动">
      {activities.map((activity, index) => {
        const references = activity.references || [];
        const key = `${activity.kind}-${index}`;
        if (activity.kind === 'query') {
          return (
            <details key={key}>
              <summary>
                <BookHeart aria-hidden="true" />
                <span>
                  <strong>{activity.label}</strong>
                  <small>{activity.hasMore ? '还有后续查询页' : '已到当前查询末页'}</small>
                </span>
                <ChevronDown aria-hidden="true" />
              </summary>
              {references.length > 0 ? (
                <ol>
                  {references.map((reference) => (
                    <li key={`${reference.memoryId}:${reference.revisionId}`}>
                      <span>{memoryReferenceLabel(reference.category, reference.importance)}</span>
                      <code title={reference.memoryId}>{reference.memoryId}</code>
                      <small title={reference.revisionId}>revision {reference.revisionId}</small>
                    </li>
                  ))}
                </ol>
              ) : <p>这次查询没有返回相关记忆正文。</p>}
            </details>
          );
        }
        if (activity.kind === 'failed' || activity.errorCode) {
          return (
            <details key={key} className={activity.kind === 'failed' ? 'failed' : undefined}>
              <summary>
                <BookHeart aria-hidden="true" />
                <span><strong>{activity.label}</strong></span>
                <ChevronDown aria-hidden="true" />
              </summary>
              <p>
                诊断信息
                {activity.errorCode && <code>{activity.errorCode}</code>}
                {activity.fieldPath && <code>{activity.fieldPath}</code>}
              </p>
            </details>
          );
        }
        return (
          <p key={key}>
            <BookHeart aria-hidden="true" />
            <span>{activity.label}</span>
            {typeof activity.count === 'number' && <small>{activity.count} 项</small>}
          </p>
        );
      })}
    </aside>
  );
}

interface StoryMessageProps {
  message: ChatMessage;
  activePersonaName?: string;
}

const StoryMessage = memo(
  function StoryMessage({ message, activePersonaName }: StoryMessageProps) {
    const action = actionLabel(message);
    const isAssistant = message.role === 'assistant';
    return (
      <article
        className={`story-message ${message.role} ${isAssistant ? 'role-voice' : ''}`}
        data-testid="story-message"
        data-message-id={message.id}
      >
        <header>
          <span>{roleLabel(message.role, activePersonaName)}</span>
          {action && <small>{action}</small>}
        </header>
        <div className="story-message-content">
          {message.content.trim() ? (
            <Suspense fallback={<p>{message.content}</p>}>
              <MarkdownMessage content={message.content} />
            </Suspense>
          ) : (
            <p>正在组织语言…</p>
          )}
          {message.streaming && <span className="message-cursor" aria-hidden="true" />}
        </div>
        {isAssistant && <MemoryActivityFeed message={message} />}
      </article>
    );
  },
  (previous, next) =>
    previous.message === next.message && previous.activePersonaName === next.activePersonaName
);

export function GalgameDialogueBox({
  messages,
  activePersonaName,
  activePersonaScenario,
  emptyText = '开始对话吧...',
  onUsePrompt,
  scrollContainerRef
}: GalgameDialogueBoxProps) {
  const dialogue = useMemo(() => visibleDialogue(messages), [messages]);
  const virtualWindow = useVirtualMessageWindow(dialogue.length, scrollContainerRef);
  const visibleMessages = dialogue.slice(virtualWindow.start, virtualWindow.end);
  const prompts = [
    `和${activePersonaName || '角色'}聊聊此刻的心情`,
    activePersonaScenario ? `从“${activePersonaScenario}”开始` : '从一个自然的开场开始',
    '请用你的方式陪我梳理今天的事'
  ];

  return (
    <section className="story-timeline" aria-label="剧情对话记录">
      {dialogue.length === 0 ? (
        <section className="story-empty-state">
          <span className="story-empty-eyebrow">新的篇章</span>
          <h1>{activePersonaName || '当前角色'}正在这里</h1>
          <p>{emptyText}</p>
          <div className="story-prompt-list" aria-label="开场建议">
            {prompts.map((prompt) => (
              <button type="button" key={prompt} onClick={() => onUsePrompt?.(prompt)}>
                {prompt}
              </button>
            ))}
          </div>
        </section>
      ) : (
        <>
          {virtualWindow.paddingBefore > 0 && (
            <div aria-hidden="true" style={{ height: virtualWindow.paddingBefore }} />
          )}
          {visibleMessages.map((message) => (
            <StoryMessage
              key={message.id}
              message={message}
              activePersonaName={activePersonaName}
            />
          ))}
          {virtualWindow.paddingAfter > 0 && (
            <div aria-hidden="true" style={{ height: virtualWindow.paddingAfter }} />
          )}
        </>
      )}
    </section>
  );
}
