import { useEffect } from 'react';
import { AlertTriangle, X } from 'lucide-react';

import type { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import type { UsePersonaStateResult } from '@/views/personas/hooks/usePersonaState';
import type { ResourceState, StoryPersonaSnapshot } from '@/views/story/types';
import { PersonaGrid } from './PersonaGrid';
import { PersonaImportDialog } from './PersonaImportDialog';
import { PersonaLibraryToolbar } from './PersonaLibraryToolbar';

interface PersonaLibraryViewProps {
  state: UsePersonaStateResult;
  controller: ReturnType<typeof usePersonaController>;
  resourceState: ResourceState<StoryPersonaSnapshot>;
  busy: boolean;
  onClose: () => void;
}

function PersonaGallerySkeleton() {
  return (
    <div className="persona-gallery-grid persona-gallery-skeleton" aria-hidden="true">
      {Array.from({ length: 8 }, (_, index) => (
        <div className="persona-gallery-card" key={index}>
          <span className="persona-skeleton-media" />
          <span className="persona-skeleton-line wide" />
          <span className="persona-skeleton-line" />
        </div>
      ))}
    </div>
  );
}

export function PersonaLibraryView({
  state,
  controller,
  resourceState,
  busy,
  onClose
}: PersonaLibraryViewProps) {
  const loading = resourceState.status === 'initial' || resourceState.status === 'loading';
  const failed = resourceState.status === 'failed';
  const syncing = resourceState.status === 'refreshing';
  const stale = resourceState.status === 'stale';

  useEffect(() => {
    const id = state.pendingPersonaFocusId;
    if (!id || state.importDialogOpen) return;
    window.requestAnimationFrame(() => {
      document.getElementById(`persona-library-card-${id}`)?.focus();
      state.clearPersonaFocusRequest();
    });
  }, [state.importDialogOpen, state.pendingPersonaFocusId]);

  function openImport() {
    controller.setPersonaImportError('');
    state.resetImportDraft();
    state.openImportDialog();
  }

  return (
    <section className="persona-library-view" aria-labelledby="persona-library-title">
      <div className="persona-library-frame">
        <header className="persona-library-heading">
          <div>
            <h1 id="persona-library-title">角色库</h1>
            <p>
              {loading
                ? '正在读取本地角色…'
                : syncing
                  ? '正在同步角色状态…'
                  : `${state.personaList.length} 个本地角色`}
            </p>
          </div>
          <button
            type="button"
            className="persona-library-close"
            aria-label="关闭角色库，返回首页"
            onClick={onClose}
          >
            <X aria-hidden="true" />
          </button>
        </header>

        <PersonaLibraryToolbar
          state={state}
          busy={busy || loading || failed}
          onImport={openImport}
          onCreate={() => void controller.openEditor('create')}
        />

        <div className="persona-library-scroll" aria-busy={loading || syncing}>
          {stale && (
            <div className="persona-library-warning" role="status">
              <AlertTriangle aria-hidden="true" />
              <span>当前显示上一次可信结果。{resourceState.error}</span>
              <button
                type="button"
                onClick={() => void controller.refreshPersonas().catch(() => undefined)}
              >
                重新同步
              </button>
            </div>
          )}
          {loading && <PersonaGallerySkeleton />}
          {failed && (
            <section className="persona-library-error" role="alert">
              <AlertTriangle aria-hidden="true" />
              <h2>无法读取角色库</h2>
              <p>{resourceState.error}</p>
              <button
                type="button"
                onClick={() => void controller.refreshPersonas().catch(() => undefined)}
              >
                重试
              </button>
            </section>
          )}
          {!loading && !failed && (
            <PersonaGrid
              state={state}
              controller={controller}
              busy={busy}
              onImport={openImport}
              onCreate={() => void controller.openEditor('create')}
            />
          )}
        </div>
      </div>
      <PersonaImportDialog state={state} controller={controller} busy={busy} />
    </section>
  );
}
