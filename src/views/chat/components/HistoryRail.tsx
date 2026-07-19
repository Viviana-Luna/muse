import { GitFork, MoreHorizontal, RotateCcw, X } from 'lucide-react';

import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import type { RuntimeSessionItem } from '@/types';

interface HistoryRailProps {
  sessions: RuntimeSessionItem[];
  activeConversationId: string;
  selectedConversationId: string;
  onSelectSession: (conversationId: string) => void | Promise<void>;
  onDeleteSession: (conversationId: string) => void | Promise<void>;
  onResumeSession: () => void | Promise<void>;
  onForkSession: () => void | Promise<void>;
  onClose: () => void;
  onNewSession: () => void | Promise<void>;
  newSessionDisabled?: boolean;
  forkDisabled?: boolean;
  resumeDisabled?: boolean;
  deleteDisabled?: boolean;
  readOnly?: boolean;
  persistent?: boolean;
}

export function sessionTitle(session: RuntimeSessionItem) {
  return (
    session.summary?.trim() ||
    session.first_prompt?.trim() ||
    (session.conversation_id === 'default' ? '未命名会话' : '新会话')
  );
}

export function sessionMeta(session: RuntimeSessionItem) {
  const parts = [`${session.records} 条记录`];
  if (session.persona_name_snapshot) {
    parts.push(
      session.persona_status === 'missing'
        ? `${session.persona_name_snapshot}（角色已删除）`
        : session.persona_name_snapshot
    );
  }
  if (session.source_conversation_id) parts.push('分叉');
  if (session.last_time) {
    const time = new Date(session.last_time);
    if (!Number.isNaN(time.getTime())) {
      parts.push(
        new Intl.DateTimeFormat('zh-CN', {
          month: '2-digit',
          day: '2-digit',
          hour: '2-digit',
          minute: '2-digit'
        }).format(time)
      );
    }
  }
  return parts.join(' · ');
}

export function HistoryRail({
  sessions,
  activeConversationId,
  selectedConversationId,
  onSelectSession,
  onDeleteSession,
  onResumeSession,
  onForkSession,
  onClose,
  onNewSession,
  newSessionDisabled,
  forkDisabled,
  resumeDisabled,
  deleteDisabled,
  readOnly = false,
  persistent = false
}: HistoryRailProps) {
  // 历史抽屉只展示已经落盘且可恢复的记录，不把当前空白会话伪装成历史记录。
  const historicalSessions = sessions.filter(
    (session) => session.can_resume && session.records > 0
  );
  const selectedSession = historicalSessions.find(
    (session) => session.conversation_id === selectedConversationId
  );
  const selectedIsActive = selectedConversationId === activeConversationId;

  return (
    <aside className="history-session-drawer" aria-label="历史会话列表">
      <header className="history-session-drawer-head">
        <span>
          <small>历史会话</small>
          <strong>{historicalSessions.length} 个聊天记录</strong>
        </span>
        {!persistent && (
          <button type="button" onClick={onClose} aria-label="关闭历史会话">
            <X aria-hidden="true" />
          </button>
        )}
      </header>

      {historicalSessions.length > 0 ? (
        <>
          {!readOnly && (
            <button
              type="button"
              className="new-session-btn"
              disabled={newSessionDisabled}
              onClick={() => void onNewSession()}
            >
              开始新对话
            </button>
          )}
          <div className="history-session-list" role="list">
            {historicalSessions.map((session) => {
              const isSelected = session.conversation_id === selectedConversationId;
              const isActive = session.conversation_id === activeConversationId;
              return (
                <article
                  className={`session-item ${isSelected ? 'active' : ''} ${isActive ? 'current' : ''}`}
                  key={session.conversation_id}
                  role="listitem"
                >
                  <button
                    type="button"
                    className="session-pick"
                    aria-label={`查看会话 ${sessionTitle(session)}，${sessionMeta(session)}`}
                    onClick={() => void onSelectSession(session.conversation_id)}
                  >
                    <strong>{isActive ? `${sessionTitle(session)} · 当前` : sessionTitle(session)}</strong>
                    <span>{sessionMeta(session)}</span>
                  </button>
                </article>
              );
            })}
          </div>
        </>
      ) : (
        <section className="history-session-drawer-empty" aria-label="历史会话为空">
          <strong>暂无历史会话</strong>
          <p>{readOnly ? '当前没有可查看的历史记录。' : '开始新对话后，聊天记录会自动保存在这里。'}</p>
          {!readOnly && (
            <button type="button" disabled={newSessionDisabled} onClick={() => void onNewSession()}>
              开始新对话
            </button>
          )}
        </section>
      )}

      {selectedSession && !readOnly && (
        <footer className="history-session-drawer-actions">
          {!selectedIsActive && (
            <>
              <button
                type="button"
                className="history-resume-action"
                disabled={resumeDisabled}
                onClick={() => void onResumeSession()}
              >
                <RotateCcw aria-hidden="true" />
                恢复会话
              </button>
              <button
                type="button"
                className="history-fork-action"
                disabled={forkDisabled}
                onClick={() => void onForkSession()}
              >
                <GitFork aria-hidden="true" />
                从此分叉
              </button>
            </>
          )}
          <details className="history-more-actions">
            <summary aria-label="更多会话操作" title="更多会话操作">
              <MoreHorizontal aria-hidden="true" />
            </summary>
            <div>
              <ConfirmDialog
                title={selectedIsActive ? '删除当前会话？' : '删除这个会话？'}
                description={
                  selectedIsActive
                    ? `“${sessionTitle(selectedSession)}”及其本地记录将被永久删除，随后会自动切换到新对话。`
                    : `“${sessionTitle(selectedSession)}”及其本地记录将被永久删除，此操作无法撤销。`
                }
                confirmLabel="删除会话"
                tone="danger"
                onConfirm={() => void onDeleteSession(selectedSession.conversation_id)}
              >
                <button type="button" disabled={deleteDisabled}>删除会话</button>
              </ConfirmDialog>
            </div>
          </details>
        </footer>
      )}
    </aside>
  );
}
