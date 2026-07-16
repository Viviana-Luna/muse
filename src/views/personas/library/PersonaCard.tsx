import type { PersonaLibraryItem } from '@/types';
import type { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import { PersonaActionsMenu } from './PersonaActionsMenu';
import { PersonaCardMedia } from './PersonaCardMedia';

interface PersonaCardProps {
  persona: PersonaLibraryItem;
  active: boolean;
  busy: boolean;
  controller: ReturnType<typeof usePersonaController>;
}

export function PersonaCard({ persona, active, busy, controller }: PersonaCardProps) {
  const identityNote = persona.author.trim()
    ? `by ${persona.author.trim()}`
    : persona.version.trim()
      ? `v${persona.version.trim().replace(/^v/i, '')}`
      : persona.id;

  return (
    <article
      id={`persona-library-card-${persona.id}`}
      className={`persona-gallery-card${active ? ' active' : ''}`}
      aria-label={persona.name}
      tabIndex={-1}
    >
      <PersonaCardMedia name={persona.name} preview={persona.visual_preview} />
      <div className="persona-gallery-card-body">
        <div className="persona-gallery-identity">
          <strong title={persona.name}>{persona.name}</strong>
          <small title={persona.author || persona.id}>{identityNote}</small>
        </div>
        <div className="persona-gallery-card-footer">
          <span className={`persona-status-chip${active ? ' active' : ''}`}>
            {active ? '当前角色' : '未启用'}
          </span>
          <button
            type="button"
            className="persona-gallery-primary-action"
            disabled={busy}
            onClick={() =>
              active
                ? void controller.openEditor('edit', persona)
                : void controller.handleActivate(persona.id)
            }
          >
            {active ? '编辑' : '启用'}
          </button>
          <PersonaActionsMenu
            name={persona.name}
            busy={busy}
            onEdit={() => void controller.openEditor('edit', persona)}
            onCopy={() => void controller.openEditor('copy', persona)}
            onExportFull={() => void controller.handleExport(persona.id, 'with_visual_pack_ref')}
            onExportLight={() => void controller.handleExport(persona.id, 'persona_only')}
            onDelete={() => void controller.handleDelete(persona.id)}
          />
        </div>
      </div>
    </article>
  );
}
