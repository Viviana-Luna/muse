import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  BookHeart,
  ChevronRight,
  ExternalLink,
  History,
  LoaderCircle,
  Plus,
  Search,
  ShieldCheck,
  Trash2
} from 'lucide-react';

import {
  adjustPersonaMemoryImportance,
  clearPersonaMemories,
  correctPersonaMemory,
  createPersonaMemory,
  deletePersonaMemory,
  fetchPersonaMemory,
  fetchPersonaMemoryHistory,
  searchPersonaMemories
} from '@/api';
import { formatApiErrorMessage } from '@/api/client';
import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import type { AppToastInput } from '@/hooks/useAppToast';
import type {
  MemoryCategory,
  MemoryDetailResponse,
  MemoryHistoryResponse,
  MemoryImportance,
  MemoryQueryItem,
  MemoryQueryPageReceipt,
  PersonaLibraryItem
} from '@/types';

const CATEGORY_LABELS: Record<MemoryCategory, string> = {
  user_fact: '用户事实',
  user_preference: '用户偏好',
  shared_experience: '共同经历',
  commitment: '约定',
  story_state: '故事状态'
};

const IMPORTANCE_LABELS: Record<MemoryImportance, string> = {
  low: '低',
  normal: '普通',
  high: '高'
};

const CHANGE_LABELS = {
  create: '新增',
  update: '更新',
  correct: '纠正'
} as const;

interface PersonaMemoryPanelProps {
  persona: PersonaLibraryItem;
  notify: (input: AppToastInput) => void;
  onBack: () => void;
  onOpenSource: (conversationId: string, turnId: string) => void;
}

interface MemoryDraft {
  category: MemoryCategory;
  importance: MemoryImportance;
  content: string;
  eventTime: string;
  changeReason: string;
}

const EMPTY_DRAFT: MemoryDraft = {
  category: 'user_fact',
  importance: 'normal',
  content: '',
  eventTime: '',
  changeReason: ''
};

function formatMemoryTime(value: string | null | undefined) {
  if (!value) return '未记录';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  }).format(date);
}

function eventTimeForRequest(value: string) {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function nextOperationId(prefix: string) {
  const suffix = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  return `${prefix}-${suffix}`.replace(/[^a-zA-Z0-9_-]/gu, '-').slice(0, 128);
}

function MemoryEditor({
  mode,
  draft,
  busy,
  onChange,
  onCancel,
  onSave
}: {
  mode: 'create' | 'correct';
  draft: MemoryDraft;
  busy: boolean;
  onChange: (draft: MemoryDraft) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  const valid = draft.content.trim().length > 0 && draft.changeReason.trim().length > 0;
  return (
    <form
      className="memory-editor"
      onSubmit={(event) => {
        event.preventDefault();
        if (valid && !busy) onSave();
      }}
    >
      <header>
        <span>
          <small>{mode === 'create' ? '手工新增' : '更正当前事实'}</small>
          <h2>{mode === 'create' ? '写下一条长期记忆' : '纠正这条记忆'}</h2>
        </span>
        <button type="button" onClick={onCancel}>取消</button>
      </header>
      <div className="memory-editor-grid">
        <label>
          类别
          <select
            value={draft.category}
            onChange={(event) => onChange({ ...draft, category: event.target.value as MemoryCategory })}
          >
            {Object.entries(CATEGORY_LABELS).map(([value, label]) => (
              <option value={value} key={value}>{label}</option>
            ))}
          </select>
        </label>
        {mode === 'create' && (
          <label>
            重要程度
            <select
              value={draft.importance}
              onChange={(event) => onChange({ ...draft, importance: event.target.value as MemoryImportance })}
            >
              {Object.entries(IMPORTANCE_LABELS).map(([value, label]) => (
                <option value={value} key={value}>{label}</option>
              ))}
            </select>
          </label>
        )}
        <label>
          发生时间（可选）
          <input
            type="datetime-local"
            value={draft.eventTime}
            onChange={(event) => onChange({ ...draft, eventTime: event.target.value })}
          />
        </label>
      </div>
      <label>
        记忆内容
        <textarea
          autoFocus
          value={draft.content}
          maxLength={2000}
          placeholder="只写一个可以独立理解的事实。"
          onChange={(event) => onChange({ ...draft, content: event.target.value })}
        />
      </label>
      <label>
        变化原因
        <textarea
          value={draft.changeReason}
          maxLength={500}
          placeholder={mode === 'create' ? '说明为什么要记住它。' : '说明原事实哪里不准确。'}
          onChange={(event) => onChange({ ...draft, changeReason: event.target.value })}
        />
      </label>
      <footer>
        <p><ShieldCheck aria-hidden="true" />保存前会经过与自动记忆相同的敏感内容检查。</p>
        <button type="submit" className="primary memory-stable-action" disabled={!valid || busy}>
          {busy ? <LoaderCircle className="memory-spin" aria-hidden="true" /> : null}
          {mode === 'create' ? '保存记忆' : '保存纠正'}
        </button>
      </footer>
    </form>
  );
}

export function PersonaMemoryPanel({
  persona,
  notify,
  onBack,
  onOpenSource
}: PersonaMemoryPanelProps) {
  const [queryDraft, setQueryDraft] = useState('');
  const [category, setCategory] = useState<MemoryCategory | ''>('');
  const [importance, setImportance] = useState<MemoryImportance | ''>('');
  const [results, setResults] = useState<MemoryQueryPageReceipt | null>(null);
  const [searchState, setSearchState] = useState<'idle' | 'loading' | 'ready' | 'failed'>('idle');
  const [searchError, setSearchError] = useState('');
  const [selectedMemoryId, setSelectedMemoryId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MemoryDetailResponse | null>(null);
  const [history, setHistory] = useState<MemoryHistoryResponse | null>(null);
  const [detailState, setDetailState] = useState<'idle' | 'loading' | 'ready' | 'failed'>('idle');
  const [detailError, setDetailError] = useState('');
  const [editorMode, setEditorMode] = useState<'create' | 'correct' | null>(null);
  const [draft, setDraft] = useState<MemoryDraft>(EMPTY_DRAFT);
  const [mutationBusy, setMutationBusy] = useState(false);
  const operationIds = useRef(new Map<string, string>());
  const searchAbort = useRef<AbortController | null>(null);

  const selectedResult = useMemo(
    () => results?.items.find((item) => item.memory_id === selectedMemoryId) ?? null,
    [results, selectedMemoryId]
  );

  useEffect(() => () => searchAbort.current?.abort(), []);

  function operationId(signature: string, prefix: string) {
    const existing = operationIds.current.get(signature);
    if (existing) return existing;
    const created = nextOperationId(prefix);
    operationIds.current.set(signature, created);
    return created;
  }

  async function performSearch(options: { cursor?: string; append?: boolean; query?: string } = {}) {
    const query = (options.query ?? queryDraft).trim();
    if (Array.from(query.replace(/[^\p{L}\p{N}]/gu, '')).length < 3) {
      setSearchState('failed');
      setSearchError('请输入至少 3 个有效字符再搜索。');
      return;
    }
    searchAbort.current?.abort();
    const controller = new AbortController();
    searchAbort.current = controller;
    setSearchState('loading');
    setSearchError('');
    try {
      const page = await searchPersonaMemories(
        persona.id,
        {
          query,
          category: category || undefined,
          importance: importance || undefined,
          cursor: options.cursor
        },
        controller.signal
      );
      if (controller.signal.aborted) return;
      setQueryDraft(query);
      setResults((current) =>
        options.append && current
          ? { ...page, items: [...current.items, ...page.items] }
          : page
      );
      setSearchState('ready');
    } catch (error) {
      if (controller.signal.aborted) return;
      setSearchState('failed');
      setSearchError(formatApiErrorMessage(error, '无法搜索长期记忆。'));
    }
  }

  async function loadDetail(memoryId: string) {
    setSelectedMemoryId(memoryId);
    setEditorMode(null);
    setDetailState('loading');
    setDetailError('');
    const controller = new AbortController();
    try {
      const [nextDetail, nextHistory] = await Promise.all([
        fetchPersonaMemory(persona.id, memoryId, controller.signal),
        fetchPersonaMemoryHistory(persona.id, memoryId, controller.signal)
      ]);
      setDetail(nextDetail);
      setHistory(nextHistory);
      setDetailState('ready');
    } catch (error) {
      if (controller.signal.aborted) return;
      setDetail(null);
      setHistory(null);
      setDetailState('failed');
      setDetailError(formatApiErrorMessage(error, '无法读取记忆详情。'));
    }
  }

  function beginCreate() {
    setSelectedMemoryId(null);
    setDetail(null);
    setHistory(null);
    setDetailState('idle');
    setDraft(EMPTY_DRAFT);
    setEditorMode('create');
  }

  function beginCorrect() {
    if (!detail) return;
    setDraft({
      category: detail.entry.category,
      importance: detail.entry.importance,
      content: detail.current_revision.content,
      eventTime: '',
      changeReason: ''
    });
    setEditorMode('correct');
  }

  async function saveEditor() {
    const payloadSignature = JSON.stringify({ mode: editorMode, selectedMemoryId, draft });
    setMutationBusy(true);
    try {
      if (editorMode === 'create') {
        const operation = operationId(payloadSignature, 'memory-create');
        const receipt = await createPersonaMemory(persona.id, {
          category: draft.category,
          content: draft.content.trim(),
          importance: draft.importance,
          event_time: eventTimeForRequest(draft.eventTime),
          change_reason: draft.changeReason.trim(),
          operation_id: operation
        });
        operationIds.current.delete(payloadSignature);
        notify({ title: '记忆已保存', description: '这条事实现在可以在后续对话中被主动查询。', tone: 'success' });
        setEditorMode(null);
        await performSearch({ query: draft.content.trim() });
        await loadDetail(receipt.memory_id);
      } else if (editorMode === 'correct' && detail) {
        const operation = operationId(payloadSignature, 'memory-correct');
        await correctPersonaMemory(persona.id, detail.entry.memory_id, {
          expected_revision_id: detail.entry.current_revision_id,
          category: draft.category,
          content: draft.content.trim(),
          event_time: eventTimeForRequest(draft.eventTime),
          change_reason: draft.changeReason.trim(),
          operation_id: operation
        });
        operationIds.current.delete(payloadSignature);
        notify({ title: '记忆已纠正', description: '旧事实只保留在管理历史中，不再参与普通查询。', tone: 'success' });
        setEditorMode(null);
        await loadDetail(detail.entry.memory_id);
        setResults((current) => current && ({
          ...current,
          items: current.items.map((item) =>
            item.memory_id === detail.entry.memory_id
              ? { ...item, content: draft.content.trim(), category: draft.category, change_type: 'correct' }
              : item
          )
        }));
      }
    } catch (error) {
      notify({ title: editorMode === 'create' ? '记忆未保存' : '记忆未纠正', description: formatApiErrorMessage(error), tone: 'error' });
    } finally {
      setMutationBusy(false);
    }
  }

  async function changeImportance(next: MemoryImportance) {
    if (!detail || next === detail.entry.importance) return;
    const signature = JSON.stringify({ kind: 'importance', id: detail.entry.memory_id, revision: detail.entry.current_revision_id, from: detail.entry.importance, to: next });
    setMutationBusy(true);
    try {
      await adjustPersonaMemoryImportance(persona.id, detail.entry.memory_id, {
        expected_revision_id: detail.entry.current_revision_id,
        expected_importance: detail.entry.importance,
        importance: next,
        operation_id: operationId(signature, 'memory-importance')
      });
      operationIds.current.delete(signature);
      setDetail({ ...detail, entry: { ...detail.entry, importance: next } });
      setResults((current) => current && ({
        ...current,
        items: current.items.map((item) => item.memory_id === detail.entry.memory_id ? { ...item, importance: next } : item)
      }));
      notify({ title: '重要程度已更新', description: `当前设为${IMPORTANCE_LABELS[next]}。`, tone: 'success' });
    } catch (error) {
      notify({ title: '重要程度未更新', description: formatApiErrorMessage(error), tone: 'error' });
    } finally {
      setMutationBusy(false);
    }
  }

  async function removeSelectedMemory() {
    if (!detail) return;
    const signature = `delete:${detail.entry.memory_id}`;
    setMutationBusy(true);
    try {
      await deletePersonaMemory(persona.id, detail.entry.memory_id, operationId(signature, 'memory-delete'));
      operationIds.current.delete(signature);
      setResults((current) => current && ({ ...current, items: current.items.filter((item) => item.memory_id !== detail.entry.memory_id) }));
      setSelectedMemoryId(null);
      setDetail(null);
      setHistory(null);
      setDetailState('idle');
      notify({ title: '记忆已永久删除', description: '来源聊天仍保留，记忆正文与全部版本已清除。', tone: 'success' });
    } catch (error) {
      notify({ title: '记忆未删除', description: formatApiErrorMessage(error), tone: 'error' });
    } finally {
      setMutationBusy(false);
    }
  }

  async function clearAll() {
    const signature = `clear:${persona.id}`;
    setMutationBusy(true);
    try {
      const receipt = await clearPersonaMemories(persona.id, operationId(signature, 'memory-clear'));
      operationIds.current.delete(signature);
      setResults(null);
      setSelectedMemoryId(null);
      setDetail(null);
      setHistory(null);
      setSearchState('idle');
      notify({ title: '角色记忆已清空', description: `已永久删除 ${receipt.deleted_memory_count} 条记忆；聊天记录未受影响。`, tone: 'success' });
    } catch (error) {
      notify({ title: '角色记忆未清空', description: formatApiErrorMessage(error), tone: 'error' });
    } finally {
      setMutationBusy(false);
    }
  }

  return (
    <section className="persona-memory-page" aria-label={`${persona.name}的长期记忆`}>
      <header className="persona-memory-head">
        <span>
          <button type="button" className="persona-memory-back" onClick={onBack}>
            <ArrowLeft aria-hidden="true" />返回角色库
          </button>
          <small>角色连续性档案</small>
          <h1>{persona.name}的记忆</h1>
          <p>只在需要时主动查询；不会把全部历史塞进每轮对话。</p>
        </span>
        <div>
          <button type="button" className="primary memory-stable-action" disabled={mutationBusy} onClick={beginCreate}>
            <Plus aria-hidden="true" />新增记忆
          </button>
          <ConfirmDialog
            title={`清空“${persona.name}”的全部记忆？`}
            description="当前事实、全部版本和来源索引都会永久删除；原始聊天记录仍会保留。"
            confirmLabel="清空全部记忆"
            tone="danger"
            confirmDisabled={mutationBusy}
            onConfirm={() => void clearAll()}
          >
            <button type="button" className="danger memory-stable-action" disabled={mutationBusy}>
              <Trash2 aria-hidden="true" />清空记忆
            </button>
          </ConfirmDialog>
        </div>
      </header>

      <div className="persona-memory-layout">
        <aside className="persona-memory-list" aria-label="记忆搜索结果">
          <form
            className="persona-memory-search"
            onSubmit={(event) => {
              event.preventDefault();
              void performSearch();
            }}
          >
            <label>
              <Search aria-hidden="true" />
              <input value={queryDraft} onChange={(event) => setQueryDraft(event.target.value)} placeholder="搜索至少 3 个字符" />
            </label>
            <div>
              <select aria-label="按记忆类别筛选" value={category} onChange={(event) => setCategory(event.target.value as MemoryCategory | '')}>
                <option value="">全部类别</option>
                {Object.entries(CATEGORY_LABELS).map(([value, label]) => <option value={value} key={value}>{label}</option>)}
              </select>
              <select aria-label="按重要程度筛选" value={importance} onChange={(event) => setImportance(event.target.value as MemoryImportance | '')}>
                <option value="">全部程度</option>
                {Object.entries(IMPORTANCE_LABELS).map(([value, label]) => <option value={value} key={value}>{label}</option>)}
              </select>
              <button type="submit" className="memory-stable-action" disabled={searchState === 'loading'}>
                {searchState === 'loading' ? <LoaderCircle className="memory-spin" aria-hidden="true" /> : <Search aria-hidden="true" />}
                搜索
              </button>
            </div>
          </form>

          <div className="persona-memory-results" aria-live="polite">
            {searchState === 'idle' && (
              <section className="persona-memory-list-empty">
                <BookHeart aria-hidden="true" />
                <strong>从一个线索开始</strong>
                <p>输入人、事或偏好，Muse 只返回相关的少量记忆。</p>
              </section>
            )}
            {searchState === 'failed' && <p className="persona-memory-error" role="alert">{searchError}</p>}
            {searchState === 'ready' && results?.items.length === 0 && (
              <section className="persona-memory-list-empty compact">
                <Search aria-hidden="true" /><strong>没有相关记忆</strong><p>换一个更具体的线索试试。</p>
              </section>
            )}
            {results?.items.map((item: MemoryQueryItem) => (
              <button
                type="button"
                key={`${item.memory_id}:${item.revision_id}`}
                className={item.memory_id === selectedMemoryId ? 'active' : ''}
                aria-current={item.memory_id === selectedMemoryId ? 'true' : undefined}
                onClick={() => void loadDetail(item.memory_id)}
              >
                <span><em>{CATEGORY_LABELS[item.category]}</em><em>{IMPORTANCE_LABELS[item.importance]}</em></span>
                <strong>{item.content}</strong>
                <small>{formatMemoryTime(item.recorded_at)} · {CHANGE_LABELS[item.change_type]}</small>
                <ChevronRight aria-hidden="true" />
              </button>
            ))}
            {results?.has_more && results.next_cursor && (
              <button
                type="button"
                className="persona-memory-more memory-stable-action"
                disabled={searchState === 'loading'}
                onClick={() => void performSearch({ cursor: results.next_cursor, append: true })}
              >
                继续查找
              </button>
            )}
          </div>
        </aside>

        <article className="persona-memory-detail" aria-label="记忆详情">
          {editorMode && (
            <MemoryEditor
              mode={editorMode}
              draft={draft}
              busy={mutationBusy}
              onChange={setDraft}
              onCancel={() => setEditorMode(null)}
              onSave={() => void saveEditor()}
            />
          )}
          {!editorMode && detailState === 'loading' && (
            <section className="persona-memory-detail-empty"><LoaderCircle className="memory-spin" aria-hidden="true" /><strong>正在读取记忆详情</strong></section>
          )}
          {!editorMode && detailState === 'failed' && (
            <section className="persona-memory-detail-empty"><BookHeart aria-hidden="true" /><strong>无法读取记忆详情</strong><p>{detailError}</p><button type="button" onClick={() => selectedMemoryId && void loadDetail(selectedMemoryId)}>重试</button></section>
          )}
          {!editorMode && detailState === 'idle' && (
            <section className="persona-memory-detail-empty"><BookHeart aria-hidden="true" /><strong>选择一条记忆</strong><p>这里会展示当前事实、来源和完整版本线。</p></section>
          )}
          {!editorMode && detail && detailState === 'ready' && (
            <>
              <header className="persona-memory-detail-head">
                <span>
                  <small>{CATEGORY_LABELS[detail.entry.category]} · 当前事实</small>
                  <h2>{detail.current_revision.content}</h2>
                  <p>{detail.current_revision.change_reason}</p>
                </span>
                <div>
                  <button type="button" disabled={mutationBusy} onClick={beginCorrect}>纠正</button>
                  <ConfirmDialog
                    title="永久删除这条记忆？"
                    description="当前正文、全部版本和来源索引都会删除；原始聊天仍保留。"
                    confirmLabel="删除记忆"
                    tone="danger"
                    confirmDisabled={mutationBusy}
                    onConfirm={() => void removeSelectedMemory()}
                  >
                    <button type="button" className="danger" disabled={mutationBusy}><Trash2 aria-hidden="true" />删除</button>
                  </ConfirmDialog>
                </div>
              </header>

              <dl className="persona-memory-facts">
                <div><dt>重要程度</dt><dd><select aria-label="调整记忆重要程度" value={detail.entry.importance} disabled={mutationBusy} onChange={(event) => void changeImportance(event.target.value as MemoryImportance)}>{Object.entries(IMPORTANCE_LABELS).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></dd></div>
                <div><dt>记录时间</dt><dd>{formatMemoryTime(detail.current_revision.recorded_at)}</dd></div>
                <div><dt>发生时间</dt><dd>{formatMemoryTime(detail.current_revision.event_time)}</dd></div>
                <div><dt>当前版本</dt><dd title={detail.entry.current_revision_id}>{detail.entry.current_revision_id.slice(0, 18)}…</dd></div>
              </dl>

              <section className="persona-memory-source">
                <span><ExternalLink aria-hidden="true" /><span><small>记忆来源</small><strong>{detail.source_conversation_id ? '来自一次真实对话' : '从角色记忆页手工保存'}</strong></span></span>
                {detail.source_conversation_id && detail.source_turn_id && (
                  <button type="button" onClick={() => onOpenSource(detail.source_conversation_id!, detail.source_turn_id!)}>打开来源回合</button>
                )}
              </section>

              <section className="persona-memory-history" aria-labelledby="persona-memory-history-title">
                <header><History aria-hidden="true" /><span><small>版本线</small><h3 id="persona-memory-history-title">这条记忆如何变化</h3></span></header>
                <ol>
                  {history?.revisions.map((revision) => (
                    <li key={revision.revision_id} className={revision.state}>
                      <span aria-hidden="true" />
                      <article>
                        <header><strong>{CHANGE_LABELS[revision.change_type]}</strong><time>{formatMemoryTime(revision.recorded_at)}</time></header>
                        <p>{revision.content}</p>
                        <small>{revision.change_reason}</small>
                        {revision.state === 'corrected' && <em>已纠正，不参与普通查询</em>}
                      </article>
                    </li>
                  ))}
                </ol>
              </section>
            </>
          )}
          {!editorMode && selectedResult && !detail && detailState === 'ready' && <p>{selectedResult.content}</p>}
        </article>
      </div>
    </section>
  );
}
