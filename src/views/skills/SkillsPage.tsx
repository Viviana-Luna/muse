import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from 'react';
import { ArrowLeft, Eye, FileText, Plus, Save, Search, Sparkles, Trash2 } from 'lucide-react';
import { createSkill, deleteSkill, getSkill, listSkills, updateSkill } from '@/api';
import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import { SectionLoading } from '@/components/feedback/LoadingState';
import type { AppToastInput } from '@/hooks/useAppToast';
import { validateSkillName, type SkillDraft, type SkillRecord, type SkillSummary } from '@/types';

const EMPTY_SKILL: SkillDraft = {
  name: '',
  description: '',
  content: '# 工作流\n\n描述这个 Skill 应该如何完成任务。',
  enabled: true
};

const MarkdownMessage = lazy(() =>
  import('@/views/chat/components/MarkdownMessage').then((module) => ({
    default: module.MarkdownMessage
  }))
);

interface SkillsPageProps {
  selectedName?: string;
  onSelectedNameChange: (name?: string) => void;
  notify: (input: AppToastInput) => void;
}

function toDraft(skill: SkillRecord): SkillDraft {
  return {
    name: skill.name,
    description: skill.description,
    content: skill.content,
    enabled: skill.enabled
  };
}

export function SkillsPage({ selectedName, onSelectedNameChange, notify }: SkillsPageProps) {
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [record, setRecord] = useState<SkillRecord | null>(null);
  const [draft, setDraft] = useState<SkillDraft>(EMPTY_SKILL);
  const [query, setQuery] = useState('');
  const [mode, setMode] = useState<'edit' | 'preview'>('edit');
  const [creating, setCreating] = useState(false);
  const [listLoading, setListLoading] = useState(true);
  const [recordLoading, setRecordLoading] = useState(false);
  const [saving, setSaving] = useState(false);

  const dirty = useMemo(() => {
    if (creating) return JSON.stringify(draft) !== JSON.stringify(EMPTY_SKILL);
    return record ? JSON.stringify(draft) !== JSON.stringify(toDraft(record)) : false;
  }, [creating, draft, record]);
  const filteredSkills = skills.filter((skill) =>
    `${skill.name} ${skill.description}`.toLowerCase().includes(query.trim().toLowerCase())
  );
  const showRecordLoading = Boolean(selectedName && !creating && recordLoading);

  const reloadList = useCallback(async () => {
    const next = await listSkills();
    setSkills(next);
    return next;
  }, []);

  useEffect(() => {
    let cancelled = false;
    setListLoading(true);
    void reloadList()
      .then((items) => {
        if (cancelled) return;
        if (!selectedName && items.length > 0) onSelectedNameChange(items[0].name);
      })
      .catch((error: Error) =>
        notify({ title: '无法读取 Skill', description: error.message, tone: 'error' })
      )
      .finally(() => {
        if (!cancelled) setListLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [reloadList]);

  useEffect(() => {
    if (!selectedName || creating) return;
    setRecordLoading(true);
    void getSkill(selectedName)
      .then((next) => {
        setRecord(next);
        setDraft(toDraft(next));
      })
      .catch((error: Error) =>
        notify({ title: '无法读取 Skill', description: error.message, tone: 'error' })
      )
      .finally(() => setRecordLoading(false));
  }, [selectedName, creating, notify]);

  function requestSelection(name: string) {
    if (dirty && !window.confirm('当前 Skill 有未保存更改，确定放弃并切换吗？')) return;
    setCreating(false);
    setRecordLoading(true);
    setMode('edit');
    onSelectedNameChange(name);
  }

  function beginCreate() {
    if (dirty && !window.confirm('当前 Skill 有未保存更改，确定放弃并新建吗？')) return;
    setCreating(true);
    setRecordLoading(false);
    setRecord(null);
    setDraft(EMPTY_SKILL);
    setMode('edit');
    onSelectedNameChange(undefined);
  }

  async function save() {
    const nameError = validateSkillName(draft.name);
    if (nameError) {
      notify({ title: 'Skill 名称无效', description: nameError, tone: 'error' });
      return;
    }
    setSaving(true);
    try {
      const saved = creating
        ? await createSkill(draft)
        : await updateSkill(record!.name, { ...draft, revision: record!.revision });
      setCreating(false);
      setRecord(saved);
      setDraft(toDraft(saved));
      await reloadList();
      onSelectedNameChange(saved.name);
      notify({ title: 'Skill 已保存', description: saved.name, tone: 'success' });
    } catch (error) {
      notify({ title: 'Skill 保存失败', description: (error as Error).message, tone: 'error' });
    } finally {
      setSaving(false);
    }
  }

  async function remove() {
    if (!record) return;
    try {
      await deleteSkill(record.name, record.revision);
      const next = await reloadList();
      setRecord(null);
      setDraft(EMPTY_SKILL);
      onSelectedNameChange(next[0]?.name);
      notify({ title: 'Skill 已删除', description: record.name, tone: 'success' });
    } catch (error) {
      notify({ title: 'Skill 删除失败', description: (error as Error).message, tone: 'error' });
    }
  }

  function showList() {
    if (dirty && !window.confirm('当前 Skill 有未保存更改，确定放弃并返回列表吗？')) return;
    setCreating(false);
    setRecordLoading(false);
    setRecord(null);
    onSelectedNameChange(undefined);
  }

  return (
    <section className={`management-page skills-page ${creating || record || selectedName ? 'show-detail' : 'show-list'}`} aria-label="Skill 管理">
      <aside className="object-list-pane">
        <header>
          <span>
            <small>本地创建</small>
            <strong>Skill</strong>
          </span>
          <button type="button" className="accent-outline" onClick={beginCreate}>
            <Plus aria-hidden="true" />
            新建
          </button>
        </header>
        <label className="management-search">
          <Search aria-hidden="true" />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索 Skill" />
        </label>
        <div className="object-list" role="list">
          {listLoading && skills.length === 0 && (
            <SectionLoading label="正在读取 Skill 列表" variant="surface" />
          )}
          {filteredSkills.map((skill) => (
            <button
              type="button"
              key={skill.name}
              className={!creating && selectedName === skill.name ? 'active' : ''}
              onClick={() => requestSelection(skill.name)}
              role="listitem"
            >
              <Sparkles aria-hidden="true" />
              <span>
                <strong>{skill.name}</strong>
                <small>{skill.description}</small>
              </span>
              <i className={skill.enabled ? 'enabled' : ''}>{skill.enabled ? '启用' : '停用'}</i>
            </button>
          ))}
          {!listLoading && filteredSkills.length === 0 && (
            <div className="object-list-empty">
              <Sparkles aria-hidden="true" />
              <strong>{skills.length === 0 ? '还没有 Skill' : '没有匹配结果'}</strong>
              <span>{skills.length === 0 ? '点击“新建”，在 Muse 中创建第一个 Skill。' : '换个关键词试试。'}</span>
            </div>
          )}
        </div>
      </aside>

      <section className="object-detail-pane">
        {showRecordLoading ? (
          <SectionLoading
            label="正在加载 Skill"
            description={selectedName}
          />
        ) : creating || record ? (
          <>
            <header className="detail-toolbar">
              <span>
                <button type="button" className="mobile-list-back" aria-label="返回 Skill 列表" onClick={showList}><ArrowLeft aria-hidden="true" /></button>
                <FileText aria-hidden="true" />
                <strong>{creating ? '新建 Skill' : record?.name}</strong>
                {dirty && <small>未保存</small>}
              </span>
              <div>
                <button type="button" className={mode === 'edit' ? 'active' : ''} onClick={() => setMode('edit')}>编辑</button>
                <button type="button" className={mode === 'preview' ? 'active' : ''} onClick={() => setMode('preview')}>
                  <Eye aria-hidden="true" />预览
                </button>
              </div>
            </header>
            <div className="skill-detail-body">
              <section className="skill-meta-grid">
                <label>
                  <span>名称</span>
                  <input
                    value={draft.name}
                    maxLength={64}
                    pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
                    aria-invalid={Boolean(validateSkillName(draft.name))}
                    title="仅允许小写字母、数字和单连字符，例如 git-release"
                    onChange={(event) => setDraft({ ...draft, name: event.target.value })}
                  />
                </label>
                <label className="management-toggle-field">
                  <span>启用状态</span>
                  <button
                    type="button"
                    className={`switch ${draft.enabled ? 'on' : ''}`}
                    aria-pressed={draft.enabled}
                    onClick={() => setDraft({ ...draft, enabled: !draft.enabled })}
                  ><i /></button>
                </label>
                <label className="wide">
                  <span>描述</span>
                  <textarea value={draft.description} maxLength={1024} rows={2} onChange={(event) => setDraft({ ...draft, description: event.target.value })} />
                </label>
              </section>
              {mode === 'edit' ? (
                <label className="skill-editor">
                  <span>SKILL.md 正文</span>
                  <textarea value={draft.content} spellCheck={false} onChange={(event) => setDraft({ ...draft, content: event.target.value })} />
                  <small>{new Blob([draft.content]).size} / 131072 字节</small>
                </label>
              ) : (
                <article className="skill-preview">
                  <Suspense fallback={<SectionLoading label="正在生成预览" variant="inline" />}>
                    <MarkdownMessage content={draft.content} />
                  </Suspense>
                </article>
              )}
            </div>
            <footer className="detail-actions">
              {!creating && record && (
                <ConfirmDialog
                  title="删除这个 Skill？"
                  description={`“${record.name}”将从 Muse 的 Skill 列表中永久删除。`}
                  confirmLabel="删除"
                  tone="danger"
                  onConfirm={() => void remove()}
                >
                  <button type="button" className="danger-ghost"><Trash2 aria-hidden="true" />删除</button>
                </ConfirmDialog>
              )}
              <span />
              <button type="button" className="primary" disabled={saving || !dirty} onClick={() => void save()}>
                <Save aria-hidden="true" />{saving ? '保存中…' : '保存更改'}
              </button>
            </footer>
          </>
        ) : (
          <div className="management-empty-detail">
            <Sparkles aria-hidden="true" />
            <h1>创建你的第一个 Skill</h1>
            <p>Skill 由名称、描述和工作流正文组成，创建后即可由 Muse 在需要时读取。</p>
            <button type="button" className="primary" onClick={beginCreate}><Plus aria-hidden="true" />新建 Skill</button>
          </div>
        )}
      </section>
    </section>
  );
}
