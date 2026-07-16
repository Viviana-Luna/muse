import { LibraryBig, SearchX } from 'lucide-react';

import type { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import type { UsePersonaStateResult } from '@/views/personas/hooks/usePersonaState';
import { PersonaCard } from './PersonaCard';

interface PersonaGridProps {
  state: UsePersonaStateResult;
  controller: ReturnType<typeof usePersonaController>;
  busy: boolean;
  onImport: () => void;
  onCreate: () => void;
}

export function PersonaGrid({ state, controller, busy, onImport, onCreate }: PersonaGridProps) {
  if (state.personaList.length === 0) {
    return (
      <section className="persona-library-empty" aria-labelledby="persona-library-empty-title">
        <LibraryBig aria-hidden="true" />
        <div>
          <h2 id="persona-library-empty-title">角色库还是空的</h2>
          <p>创建一个新角色，或从 Muse JSON 角色卡导入已有角色。</p>
          <span>
            <button type="button" className="persona-library-create-action" onClick={onCreate}>
              创建角色
            </button>
            <button type="button" onClick={onImport}>导入角色卡</button>
          </span>
        </div>
      </section>
    );
  }

  if (state.filteredPersonas.length === 0) {
    return (
      <section className="persona-library-no-results" aria-labelledby="persona-no-results-title">
        <SearchX aria-hidden="true" />
        <h2 id="persona-no-results-title">没有符合条件的角色</h2>
        <p>尝试更换关键词，或清除当前筛选条件。</p>
        <button
          type="button"
          onClick={() => {
            state.setSearch('');
            state.setStatusFilter('all');
          }}
        >
          清除筛选
        </button>
      </section>
    );
  }

  return (
    <div className="persona-gallery-grid" aria-label="本地角色列表">
      {state.filteredPersonas.map((persona) => (
        <PersonaCard
          key={persona.id}
          persona={persona}
          active={persona.id === state.activePersonaId}
          busy={busy}
          controller={controller}
        />
      ))}
    </div>
  );
}
