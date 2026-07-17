import { useEffect, useMemo, useState } from 'react';
import {
  Archive,
  ArchiveRestore,
  ArrowRight,
  BookOpenText,
  Download,
  Eye,
  GitFork,
  MessageSquarePlus,
  PencilLine,
  RotateCcw,
  Search,
  Trash2
} from 'lucide-react';

import {
  exportRuntimeSession,
  fetchHistory,
  fetchRuntimeSessionContext,
  fetchRuntimeSessions,
  updateRuntimeSessionMetadata
} from '@/api';
import { SectionLoading } from '@/components/feedback/LoadingState';
import { GalgameDialogueBox } from '@/views/chat/components/GalgameDialogueBox';
import { sessionMeta, sessionTitle } from '@/views/chat/components/HistoryRail';
import type { useChatRuntime } from '@/views/chat/hooks/useChatRuntime';
import { toChatMessages, type ChatMessage } from '@/hooks/useRuntimeStream';
import type { RuntimeSessionContextResponse } from '@/types';

type ChatRuntime = ReturnType<typeof useChatRuntime>;

interface SessionsPageProps {
  runtime: ChatRuntime;
  activePersonaId?: string;
  activePersonaName?: string;
  onOpenChat: () => void;
}

function preferenceSourceLabel(value: unknown): string {
  if (value === 'persona_preference') return '角色偏好';
  if (value === 'global_active') return '全局活动配置';
  if (value === 'unavailable') return '不可用';
  return typeof value === 'string' && value ? value : '未记录';
}

export function SessionsPage({
  runtime,
  activePersonaId,
  activePersonaName,
  onOpenChat
}: SessionsPageProps) {
  const [query, setQuery] = useState('');
  const [showArchived, setShowArchived] = useState(false);
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState('');
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [context, setContext] = useState<RuntimeSessionContextResponse | null>(null);
  const [selectedConversationId, setSelectedConversationId] = useState(
    runtime.activeConversationId
  );
  const [previewMessages, setPreviewMessages] = useState<ChatMessage[]>([]);
  const [previewState, setPreviewState] = useState<'idle' | 'loading' | 'ready' | 'failed'>('idle');
  const sessions = useMemo(
    () => runtime.runtimeSessions.filter((session) => session.can_resume && session.records > 0),
    [runtime.runtimeSessions]
  );
  const selectedSession = sessions.find(
    (session) => session.conversation_id === selectedConversationId
  );
  const filteredSessions = sessions.filter(
    (session) =>
      (showArchived || !session.archived) &&
      `${sessionTitle(session)} ${session.first_prompt || ''} ${session.persona_name_snapshot || ''} ${session.conversation_id}`
        .toLowerCase()
        .includes(query.trim().toLowerCase())
  );
  const hasMessages = previewMessages.some(
    (message) => message.role !== 'system' && Boolean(message.content.trim() || message.streaming)
  );

  useEffect(() => {
    if (selectedSession || !sessions[0]) return;
    setSelectedConversationId(sessions[0].conversation_id);
  }, [selectedSession, sessions]);

  useEffect(() => {
    if (!selectedSession) {
      setPreviewMessages([]);
      setPreviewState('idle');
      return;
    }
    const controller = new AbortController();
    setPreviewState('loading');
    void fetchHistory(selectedSession.conversation_id, controller.signal)
      .then((history) => {
        setPreviewMessages(toChatMessages(history));
        setPreviewState('ready');
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setPreviewMessages([]);
        setPreviewState('failed');
        console.error(error instanceof Error ? error.message : '读取会话详情失败。');
      });
    return () => controller.abort();
  }, [selectedSession]);

  useEffect(() => {
    setEditingTitle(false);
    setTitleDraft(selectedSession ? sessionTitle(selectedSession) : '');
    setInspectorOpen(false);
    setContext(null);
  }, [selectedSession?.conversation_id]);

  async function refreshSessions() {
    const response = await fetchRuntimeSessions();
    runtime.applyRuntimeSessions(
      response,
      runtime.activeConversationId,
      selectedConversationId
    );
  }

  async function saveTitle() {
    if (!selectedSession) return;
    await updateRuntimeSessionMetadata(selectedSession.conversation_id, {
      title: titleDraft.trim() || null
    });
    await refreshSessions();
    setEditingTitle(false);
    runtime.setRuntimeStatus('会话标题已保存。');
  }

  async function toggleArchive() {
    if (!selectedSession) return;
    await updateRuntimeSessionMetadata(selectedSession.conversation_id, {
      archived: !selectedSession.archived
    });
    if (!selectedSession.archived) setShowArchived(true);
    await refreshSessions();
    runtime.setRuntimeStatus(selectedSession.archived ? '会话已取消归档。' : '会话已归档。');
  }

  async function downloadExport() {
    if (!selectedSession) return;
    const payload = await exportRuntimeSession(selectedSession.conversation_id);
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `muse-session-${selectedSession.conversation_id}.json`;
    link.click();
    URL.revokeObjectURL(url);
  }

  async function toggleInspector() {
    if (!selectedSession) return;
    const nextOpen = !inspectorOpen;
    setInspectorOpen(nextOpen);
    if (!nextOpen || context) return;
    setContext(await fetchRuntimeSessionContext(selectedSession.conversation_id));
  }

  async function startNewConversation() {
    if (await runtime.startNewConversationFromHistory()) onOpenChat();
  }

  async function resumeConversation() {
    if (selectedSession && (await runtime.handleResumeSession(selectedSession.conversation_id))) {
      onOpenChat();
    }
  }

  async function forkConversation() {
    const targetPersonaId = selectedSession?.persona_status === 'missing' ? activePersonaId : undefined;
    const forked = selectedSession
      ? targetPersonaId
        ? await runtime.handleForkSession(selectedSession.conversation_id, targetPersonaId)
        : await runtime.handleForkSession(selectedSession.conversation_id)
      : false;
    if (selectedSession && forked) {
      onOpenChat();
    }
  }

  return (
    <section className="sessions-page" aria-label="会话管理">
      <header className="sessions-page-head">
        <span>
          <small>对话档案</small>
          <h1>会话</h1>
          <p>查看本地聊天记录，选择恢复原会话或从历史节点创建分叉。</p>
        </span>
        <button
          type="button"
          className="sessions-new-action primary"
          disabled={runtime.busy}
          onClick={() => void startNewConversation()}
        >
          <MessageSquarePlus aria-hidden="true" />
          开始新对话
        </button>
      </header>

      <div className="sessions-page-grid">
        <aside className="sessions-list-panel" aria-label="会话列表">
          <header>
            <span>
              <strong>{sessions.length} 个会话</strong>
              <small>仅显示已落盘且可以恢复的记录</small>
            </span>
            <button
              type="button"
              className={showArchived ? 'active' : ''}
              onClick={() => setShowArchived((value) => !value)}
            >
              <Archive aria-hidden="true" />
              {showArchived ? '隐藏归档' : '显示归档'}
            </button>
          </header>
          <label className="sessions-search">
            <Search aria-hidden="true" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索标题、开场或会话 ID"
            />
          </label>
          <div className="sessions-list" role="list">
            {filteredSessions.map((session) => {
              const selected = session.conversation_id === selectedConversationId;
              const current = session.conversation_id === runtime.activeConversationId;
              return (
                <article role="listitem" key={session.conversation_id}>
                  <button
                    type="button"
                    className={`${selected ? 'active' : ''} ${current ? 'current' : ''}`}
                    aria-label={`查看会话 ${sessionTitle(session)}，${sessionMeta(session)}`}
                    aria-current={selected ? 'true' : undefined}
                    onClick={() => setSelectedConversationId(session.conversation_id)}
                  >
                    <span>
                      <strong>{sessionTitle(session)}</strong>
                      <small>{sessionMeta(session)}</small>
                    </span>
                    {session.archived && <em>归档</em>}
                    {current && <em>当前</em>}
                  </button>
                </article>
              );
            })}
            {sessions.length === 0 && (
              <section className="sessions-list-empty">
                <BookOpenText aria-hidden="true" />
                <strong>还没有历史会话</strong>
                <p>开始一次对话后，已完成的记录会出现在这里。</p>
              </section>
            )}
            {sessions.length > 0 && filteredSessions.length === 0 && (
              <section className="sessions-list-empty compact">
                <Search aria-hidden="true" />
                <strong>没有匹配结果</strong>
                <p>换一个标题或关键词试试。</p>
              </section>
            )}
          </div>
        </aside>

        <article
          className={`sessions-preview-panel ${selectedSession ? '' : 'is-empty'}`}
          aria-label="会话详情"
        >
          {selectedSession ? (
            <>
              <header className="sessions-preview-head">
                <span>
                  <small>
                    {selectedSession.conversation_id === runtime.activeConversationId
                      ? '当前会话'
                      : selectedSession.source_conversation_id
                        ? '分叉会话'
                        : '历史会话'}
                  </small>
                  {editingTitle ? (
                    <form
                      className="sessions-title-editor"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void saveTitle();
                      }}
                    >
                      <input
                        autoFocus
                        value={titleDraft}
                        maxLength={120}
                        aria-label="会话标题"
                        onChange={(event) => setTitleDraft(event.target.value)}
                      />
                      <button type="submit">保存</button>
                      <button type="button" onClick={() => setEditingTitle(false)}>取消</button>
                    </form>
                  ) : (
                    <h2>
                      {sessionTitle(selectedSession)}
                      <button
                        type="button"
                        className="sessions-inline-action"
                        onClick={() => setEditingTitle(true)}
                        aria-label="重命名会话"
                      >
                        <PencilLine aria-hidden="true" />
                      </button>
                    </h2>
                  )}
                  <p>{sessionMeta(selectedSession)}</p>
                </span>
                <div className="sessions-preview-actions">
                  {selectedSession.conversation_id === runtime.activeConversationId ? (
                    <button type="button" className="primary" onClick={onOpenChat}>
                      打开当前对话
                      <ArrowRight aria-hidden="true" />
                    </button>
                  ) : (
                    <>
                      <button
                        type="button"
                        className="primary"
                        disabled={runtime.busy || selectedSession.persona_status === 'missing'}
                        title={
                          selectedSession.persona_status === 'missing'
                            ? '所属角色已删除，原会话只能查看或导出。'
                            : undefined
                        }
                        onClick={() => void resumeConversation()}
                      >
                        <RotateCcw aria-hidden="true" />
                        恢复并继续
                      </button>
                      <button
                        type="button"
                        disabled={
                          runtime.busy ||
                          (selectedSession.persona_status === 'missing' && !activePersonaId)
                        }
                        onClick={() => void forkConversation()}
                      >
                        <GitFork aria-hidden="true" />
                        分叉后继续
                      </button>
                    </>
                  )}
                  <button
                    type="button"
                    onClick={() => void toggleInspector()}
                    aria-pressed={inspectorOpen}
                  >
                    <Eye aria-hidden="true" />
                    上下文
                  </button>
                  <button type="button" onClick={() => void downloadExport()}>
                    <Download aria-hidden="true" />
                    导出
                  </button>
                  <button type="button" onClick={() => void toggleArchive()}>
                    {selectedSession.archived ? (
                      <ArchiveRestore aria-hidden="true" />
                    ) : (
                      <Archive aria-hidden="true" />
                    )}
                    {selectedSession.archived ? '取消归档' : '归档'}
                  </button>
                  <button
                    type="button"
                    className="sessions-delete-action"
                    disabled={runtime.busy}
                    onClick={() =>
                      void runtime.handleDeleteRuntimeSession(selectedSession.conversation_id)
                    }
                    aria-label="删除会话"
                    title="删除会话"
                  >
                    <Trash2 aria-hidden="true" />
                  </button>
                </div>
              </header>
              {inspectorOpen && (
                <section className="sessions-context-inspector" aria-label="Context Inspector">
                  <header>
                    <span><small>只读检查器</small><strong>本轮上下文来源</strong></span>
                    <button type="button" onClick={() => setInspectorOpen(false)}>关闭</button>
                  </header>
                  {context ? (
                    <div>
                      <article>
                        <small>角色与模型</small>
                        <strong>{String(context.runtime_policy_snapshot?.provider || '未记录')} / {String(context.runtime_policy_snapshot?.model || '未记录')}</strong>
                        <p>
                          来源：{preferenceSourceLabel(context.runtime_policy_snapshot?.model_source)}
                          {context.runtime_policy_snapshot?.model_fallback ? '（已回退）' : ''}
                          {' · '}角色版本：{String(context.runtime_policy_snapshot?.persona_version || '未记录')}
                        </p>
                        {typeof context.runtime_policy_snapshot?.model_fallback_reason === 'string' && (
                          <p>回退原因：{String(context.runtime_policy_snapshot.model_fallback_reason)}</p>
                        )}
                      </article>
                      <article>
                        <small>语音</small>
                        <strong>{String(context.runtime_policy_snapshot?.voice_id || '本轮不可用')}</strong>
                        <p>
                          来源：{preferenceSourceLabel(context.runtime_policy_snapshot?.voice_source)}
                          {context.runtime_policy_snapshot?.voice_fallback ? '（已回退）' : ''}
                        </p>
                        {typeof context.runtime_policy_snapshot?.voice_fallback_reason === 'string' && (
                          <p>说明：{String(context.runtime_policy_snapshot.voice_fallback_reason)}</p>
                        )}
                      </article>
                      <article>
                        <small>Tool · Skill · MCP</small>
                        <strong>{Array.isArray(context.runtime_policy_snapshot?.tool_ids) ? context.runtime_policy_snapshot.tool_ids.length : 0} 个冻结工具</strong>
                        <p>策略版本：{String(context.runtime_policy_snapshot?.policy_version || '未记录')}</p>
                      </article>
                      <article>
                        <small>Token 与片段</small>
                        <strong>{context.context_snapshot ? '已有上下文快照' : '尚无上下文快照'}</strong>
                        <p>这里展示回合开始时冻结的事实，不允许编辑。</p>
                      </article>
                    </div>
                  ) : (
                    <SectionLoading label="正在读取上下文来源" variant="inline" />
                  )}
                </section>
              )}
              <div className="sessions-preview-scroll" ref={runtime.chatScrollRef}>
                {previewState === 'loading' ? (
                  <SectionLoading
                    label="正在读取会话内容"
                    description="历史预览不会替换当前对话。"
                  />
                ) : hasMessages ? (
                  <GalgameDialogueBox
                    messages={previewMessages}
                    activePersonaName={activePersonaName || '角色'}
                    scrollContainerRef={runtime.chatScrollRef}
                  />
                ) : (
                  <section className="sessions-preview-empty">
                    <BookOpenText aria-hidden="true" />
                    <strong>{previewState === 'failed' ? '无法读取会话内容' : '该会话暂无消息'}</strong>
                    <p>
                      {previewState === 'failed'
                        ? '请重新选择该会话后再试。'
                        : '这条记录还没有可展示的对话内容。'}
                    </p>
                  </section>
                )}
              </div>
            </>
          ) : (
            <section className="sessions-preview-empty full">
              <BookOpenText aria-hidden="true" />
              <strong>选择一个会话查看详情</strong>
              <p>会话内容会在这里以只读方式展示，不会影响当前对话。</p>
            </section>
          )}
        </article>
      </div>
    </section>
  );
}
