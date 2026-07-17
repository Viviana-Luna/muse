import { useEffect, useId, useMemo, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, RefreshCw, Search, Sparkles, X } from 'lucide-react';

import { useModalAccessibility } from '@/hooks/useModalAccessibility';
import type { RuntimeSkillSummary } from '@/types';

interface SkillPickerDrawerProps {
  open: boolean;
  skills: RuntimeSkillSummary[];
  loading: boolean;
  error: string | null;
  omittedSkillCount: number;
  selectedName?: string;
  onSelect: (skill: RuntimeSkillSummary) => void;
  onClose: () => void;
  onRetry: () => void | Promise<void>;
}

export function SkillPickerDrawer({
  open,
  skills,
  loading,
  error,
  omittedSkillCount,
  selectedName,
  onSelect,
  onClose,
  onRetry
}: SkillPickerDrawerProps) {
  const titleId = useId();
  const descriptionId = useId();
  const [query, setQuery] = useState('');
  const dialogRef = useModalAccessibility<HTMLDivElement>(open, onClose);

  useEffect(() => {
    if (open) setQuery('');
  }, [open]);

  const filteredSkills = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    if (!normalizedQuery) return skills;
    return skills.filter(
      (skill) =>
        skill.name.toLocaleLowerCase().includes(normalizedQuery) ||
        skill.description.toLocaleLowerCase().includes(normalizedQuery)
    );
  }, [query, skills]);

  if (!open) return null;
  const portalTarget = document.querySelector<HTMLElement>('.story-shell') ?? document.body;

  return createPortal(
    <div
      id="skill-picker-drawer"
      ref={dialogRef}
      className="skill-picker-layer"
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={descriptionId}
      tabIndex={-1}
    >
      <div className="skill-picker-scrim" aria-hidden="true" onMouseDown={onClose} />
      <section className="skill-picker-drawer">
        <header>
          <div>
            <span className="skill-picker-eyebrow">本轮工作流</span>
            <h2 id={titleId}>选择 Skill</h2>
            <p id={descriptionId}>选择后会作为附件加入输入区，并在发送时真实激活。</p>
          </div>
          <button type="button" className="skill-picker-close" onClick={onClose} aria-label="关闭 Skill 选择器">
            <X aria-hidden="true" />
          </button>
        </header>

        <label className="skill-picker-search">
          <Search aria-hidden="true" />
          <span className="sr-only">搜索 Skill</span>
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索名称或用途"
            autoComplete="off"
          />
        </label>

        <div className="skill-picker-content" aria-busy={loading || undefined}>
          {loading && skills.length === 0 && (
            <div className="skill-picker-state" role="status">
              <RefreshCw className="is-spinning" aria-hidden="true" />
              <span>正在读取当前角色可用的 Skill…</span>
            </div>
          )}
          {error && skills.length === 0 && (
            <div className="skill-picker-state is-error" role="alert">
              <span>{error}</span>
              <button type="button" onClick={() => void onRetry()}>
                <RefreshCw aria-hidden="true" />
                重试
              </button>
            </div>
          )}
          {!loading && !error && skills.length === 0 && (
            <div className="skill-picker-state">
              <Sparkles aria-hidden="true" />
              <span>当前角色没有可用的 Skill。</span>
            </div>
          )}
          {skills.length > 0 && filteredSkills.length === 0 && (
            <div className="skill-picker-state">
              <Search aria-hidden="true" />
              <span>没有匹配“{query.trim()}”的 Skill。</span>
            </div>
          )}
          {filteredSkills.length > 0 && (
            <ul className="skill-picker-list" aria-label="可用 Skill">
              {filteredSkills.map((skill) => {
                const selected = skill.name === selectedName;
                return (
                  <li key={`${skill.name}:${skill.revision}`}>
                    <button
                      type="button"
                      className={`skill-picker-item${selected ? ' is-selected' : ''}`}
                      onClick={() => onSelect(skill)}
                      aria-pressed={selected}
                    >
                      <span className="skill-picker-item-icon">
                        <Sparkles aria-hidden="true" />
                      </span>
                      <span className="skill-picker-item-copy">
                        <strong>{skill.name}</strong>
                        <span>{skill.description}</span>
                      </span>
                      {selected && <Check className="skill-picker-check" aria-hidden="true" />}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        {(omittedSkillCount > 0 || (loading && skills.length > 0) || (error && skills.length > 0)) && (
          <footer role="status">
            {omittedSkillCount > 0 && <span>另有 {omittedSkillCount} 个 Skill 因目录预算未展示。</span>}
            {loading && skills.length > 0 && <span>正在刷新目录…</span>}
            {error && skills.length > 0 && (
              <button type="button" onClick={() => void onRetry()}>刷新失败，重试</button>
            )}
          </footer>
        )}
      </section>
    </div>,
    portalTarget
  );
}
