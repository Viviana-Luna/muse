import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { FileJson, Upload, X } from 'lucide-react';

import { useModalAccessibility } from '@/hooks/useModalAccessibility';
import type { PersonaCard } from '@/types';
import type { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import type {
  PersonaImportConflictStrategy,
  UsePersonaStateResult
} from '@/views/personas/hooks/usePersonaState';
import { PersonaCardMedia } from './PersonaCardMedia';

interface PersonaImportDialogProps {
  state: UsePersonaStateResult;
  controller: ReturnType<typeof usePersonaController>;
  busy: boolean;
}

function parsePreview(text: string): PersonaCard | null {
  if (!text.trim()) return null;
  try {
    const value = JSON.parse(text) as PersonaCard;
    return value && typeof value === 'object' && value.persona ? value : null;
  } catch {
    return null;
  }
}

function safeLocalPreviewPath(path: string | undefined): string | null {
  const normalized = path?.trim() ?? '';
  return normalized.startsWith('/assets/') || normalized.startsWith('/api/assets/')
    ? normalized
    : null;
}

export function PersonaImportDialog({ state, controller, busy }: PersonaImportDialogProps) {
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const errorRef = useRef<HTMLParagraphElement | null>(null);
  const dialogRef = useModalAccessibility<HTMLDivElement>(
    state.importDialogOpen,
    handleClose
  );
  const preview = useMemo(() => parsePreview(state.importText), [state.importText]);
  const previewVisual = {
    avatar_path: safeLocalPreviewPath(preview?.visual_pack?.avatar_path),
    portrait_path: safeLocalPreviewPath(preview?.visual_pack?.portrait_path)
  };

  useEffect(() => {
    if (!controller.personaImportError) return;
    setAdvancedOpen(true);
    window.requestAnimationFrame(() => errorRef.current?.focus());
  }, [controller.personaImportError]);

  if (!state.importDialogOpen) return null;

  function handleClose() {
    if (busy) return;
    controller.setPersonaImportError('');
    state.resetImportDraft();
    state.closeImportDialog();
  }

  const dialog = (
    <div
      ref={dialogRef}
      className="persona-import-layer"
      role="dialog"
      aria-modal="true"
      aria-labelledby="persona-import-title"
      aria-busy={busy}
      tabIndex={-1}
    >
      <section className="persona-import-dialog">
        <header>
          <div>
            <span>从文件添加</span>
            <h2 id="persona-import-title">导入角色卡</h2>
            <p>先检查角色信息，再决定重名处理方式和是否立即启用。</p>
          </div>
          <button type="button" aria-label="关闭导入角色卡" disabled={busy} onClick={handleClose}>
            <X aria-hidden="true" />
          </button>
        </header>

        <div className="persona-import-content">
          <input
            ref={state.personaCardInputRef}
            className="sr-only"
            type="file"
            accept="application/json,.json,.muse-role-card.json"
            onChange={controller.handlePersonaCardFile}
          />
          <button
            type="button"
            className={`persona-import-file${state.importText.trim() ? ' ready' : ''}`}
            disabled={busy}
            onClick={() => state.personaCardInputRef.current?.click()}
          >
            {state.importText.trim() ? <FileJson aria-hidden="true" /> : <Upload aria-hidden="true" />}
            <span>
              <strong>{state.importFileName || '选择角色卡文件'}</strong>
              <small>
                {state.importText.trim()
                  ? '文件已读取，可以检查并导入'
                  : '支持 .json 和 .muse-role-card.json'}
              </small>
            </span>
          </button>

          {preview ? (
            <article className="persona-import-preview">
              <PersonaCardMedia name={preview.persona.name} preview={previewVisual} />
              <div>
                <span>即将导入</span>
                <strong>{preview.persona.name}</strong>
                <small>{preview.persona.author || preview.persona.id}</small>
                <p>{preview.persona.summary || '这张角色卡没有填写简介。'}</p>
              </div>
            </article>
          ) : state.importText.trim() ? (
            <p className="persona-import-invalid">当前内容尚不能识别为 Muse 角色卡，请检查 JSON。</p>
          ) : null}

          <div className="persona-import-options">
            <label>
              <span>重名处理</span>
              <select
                value={state.importConflictStrategy}
                onChange={(event) =>
                  state.setImportConflictStrategy(
                    event.target.value as PersonaImportConflictStrategy
                  )
                }
              >
                <option value="rename">创建副本</option>
                <option value="overwrite">覆盖本地角色</option>
                <option value="cancel">遇到重名时取消</option>
              </select>
            </label>
            <label className="persona-import-activate">
              <input
                type="checkbox"
                checked={state.activateAfterImport}
                onChange={(event) => state.setActivateAfterImport(event.target.checked)}
              />
              <span>
                <strong>导入后启用</strong>
                <small>成功后立即切换为当前角色</small>
              </span>
            </label>
          </div>

          <details
            className="persona-import-advanced"
            open={advancedOpen}
            onToggle={(event) => setAdvancedOpen(event.currentTarget.open)}
          >
            <summary>高级导入：粘贴 JSON</summary>
            <label>
              <span className="sr-only">角色卡 JSON</span>
              <textarea
                value={state.importText}
                aria-invalid={Boolean(controller.personaImportError)}
                placeholder="粘贴 Muse 角色卡 JSON"
                onChange={(event) => {
                  state.setImportText(event.target.value);
                  state.setImportFileName('');
                  controller.setPersonaImportError('');
                }}
              />
            </label>
          </details>

          {controller.personaImportError && (
            <p ref={errorRef} className="field-error" role="alert" tabIndex={-1}>
              {controller.personaImportError}
            </p>
          )}
        </div>

        <footer>
          <button type="button" disabled={busy} onClick={handleClose}>取消</button>
          <button
            type="button"
            className="persona-import-submit"
            disabled={!preview || busy}
            onClick={() => void controller.handleImport()}
          >
            {busy ? '正在导入…' : '导入角色卡'}
          </button>
        </footer>
      </section>
      <button
        type="button"
        className="persona-import-backdrop"
        aria-label="关闭导入角色卡"
        tabIndex={-1}
        disabled={busy}
        onClick={handleClose}
      />
    </div>
  );

  const portalHost = document.querySelector<HTMLElement>('.runtime-shell') ?? document.body;
  return createPortal(dialog, portalHost);
}
