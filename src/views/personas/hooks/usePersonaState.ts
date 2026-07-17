import { useMemo, useRef, useState } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';

import type {
  ActivePersonaResponse,
  Persona,
  PersonaLibraryItem,
  PersonaVisualPackPatch,
  VisualPack
} from '@/types';

export type PersonaEditorMode = 'create' | 'edit' | 'copy';
export type PersonaStatusFilter = 'all' | 'active' | 'inactive';
export type PersonaImportConflictStrategy = 'cancel' | 'overwrite' | 'rename';

export interface PersonaEditorState {
  open: boolean;
  mode: PersonaEditorMode;
  persona: Persona;
  visualPackDraft: PersonaVisualPackPatch;
  visualDirty: boolean;
}

export interface UsePersonaStateOptions {
  initialPersona: Persona;
  initialVisualPackDraft: PersonaVisualPackPatch;
}

export const DEFAULT_PERSONA: Persona = {
  id: '',
  name: '',
  summary: '',
  character_profile: '',
  world_profile: '现实日常',
  scenario: '',
  system_prompt: '请稳定扮演当前角色，与用户进行持续对话。',
  style: '',
  roleplay_style: 'light_narration',
  dialogue_examples: '',
  author_note: '',
  opening_message: '',
  tool_policy: {
    mode: 'inherit',
    allowed_tools: []
  },
  skill_policy: {
    mode: 'inherit',
    allowed_skills: []
  },
  mcp_policy: {
    mode: 'inherit',
    allowed_servers: []
  },
  preferred_model_ref: null,
  preferred_voice_id: null,
  default_visual_pack_id: 'default-visual-pack',
  author: '',
  version: '1.0.0',
  notes: ''
};

const DEFAULT_PERSONA_THEME_COLOR = '#d8596f';
const DEFAULT_PORTRAIT_FRAME = 'portrait';
const DEFAULT_PORTRAIT_FIT = 'cover';
const DEFAULT_PORTRAIT_POSITION = 50;
const DEFAULT_PORTRAIT_SCALE = 100;

function clampVisualNumber(
  value: number | undefined,
  min: number,
  max: number,
  fallback: number
): number {
  if (typeof value !== 'number' || Number.isNaN(value)) return fallback;
  return Math.min(max, Math.max(min, value));
}

export function createPersonaVisualDraft(
  visualPack: VisualPack | null
): PersonaVisualPackPatch {
  return {
    portrait_path: visualPack?.portrait_path ?? '',
    background_path: visualPack?.background_path ?? '',
    avatar_path: visualPack?.avatar_path ?? '',
    theme_color: visualPack?.theme_color || DEFAULT_PERSONA_THEME_COLOR,
    theme_mode:
      visualPack?.theme_mode === 'dark' || visualPack?.theme_mode === 'light'
        ? visualPack.theme_mode
        : 'auto',
    portrait_frame: visualPack?.portrait_frame || DEFAULT_PORTRAIT_FRAME,
    portrait_fit: visualPack?.portrait_fit || DEFAULT_PORTRAIT_FIT,
    portrait_position_x: clampVisualNumber(
      visualPack?.portrait_position_x,
      0,
      100,
      DEFAULT_PORTRAIT_POSITION
    ),
    portrait_position_y: clampVisualNumber(
      visualPack?.portrait_position_y,
      0,
      100,
      DEFAULT_PORTRAIT_POSITION
    ),
    portrait_scale: clampVisualNumber(
      visualPack?.portrait_scale,
      70,
      180,
      DEFAULT_PORTRAIT_SCALE
    )
  };
}

export interface UsePersonaStateResult {
  personaList: PersonaLibraryItem[];
  activePersonaId: string | null;
  activePersona: Persona | null;
  activeVisualPack: VisualPack | null;
  search: string;
  setSearch: Dispatch<SetStateAction<string>>;
  statusFilter: PersonaStatusFilter;
  setStatusFilter: Dispatch<SetStateAction<PersonaStatusFilter>>;
  filteredPersonas: PersonaLibraryItem[];
  editor: PersonaEditorState;
  setEditor: Dispatch<SetStateAction<PersonaEditorState>>;
  closeEditor: () => void;
  importDialogOpen: boolean;
  openImportDialog: () => void;
  closeImportDialog: () => void;
  importText: string;
  setImportText: Dispatch<SetStateAction<string>>;
  importFileName: string;
  setImportFileName: Dispatch<SetStateAction<string>>;
  importConflictStrategy: PersonaImportConflictStrategy;
  setImportConflictStrategy: Dispatch<SetStateAction<PersonaImportConflictStrategy>>;
  activateAfterImport: boolean;
  setActivateAfterImport: Dispatch<SetStateAction<boolean>>;
  personaCardInputRef: MutableRefObject<HTMLInputElement | null>;
  resetImportDraft: () => void;
  pendingPersonaFocusId: string | null;
  requestPersonaFocus: (id: string) => void;
  clearPersonaFocusRequest: () => void;
  applyPersonaList: (personas: PersonaLibraryItem[], currentId: string | null) => void;
  applyActivePersona: (payload: ActivePersonaResponse) => void;
  clearActivePersona: () => void;
  updateEditor: <K extends keyof Persona>(key: K, value: Persona[K]) => void;
  updateEditorVisualDraft: <K extends keyof PersonaVisualPackPatch>(
    key: K,
    value: PersonaVisualPackPatch[K]
  ) => void;
  updateEditorToolPolicyMode: (mode: Persona['tool_policy']['mode']) => void;
  updateEditorAllowedTools: (value: string) => void;
}

export function usePersonaState({
  initialPersona = DEFAULT_PERSONA,
  initialVisualPackDraft = createPersonaVisualDraft(null)
}: Partial<UsePersonaStateOptions> = {}): UsePersonaStateResult {
  const [personaList, setPersonaList] = useState<PersonaLibraryItem[]>([]);
  const [activePersonaId, setActivePersonaId] = useState<string | null>(null);
  const [activePersona, setActivePersona] = useState<Persona | null>(null);
  const [activeVisualPack, setActiveVisualPack] = useState<VisualPack | null>(null);
  const [search, setSearch] = useState('');
  const [statusFilter, setStatusFilter] = useState<PersonaStatusFilter>('all');
  const [editor, setEditor] = useState<PersonaEditorState>({
    open: false,
    mode: 'create',
    persona: initialPersona,
    visualPackDraft: initialVisualPackDraft,
    visualDirty: false
  });
  const [importText, setImportText] = useState('');
  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [importFileName, setImportFileName] = useState('');
  const [importConflictStrategy, setImportConflictStrategy] =
    useState<PersonaImportConflictStrategy>('rename');
  const [activateAfterImport, setActivateAfterImport] = useState(true);
  const [pendingPersonaFocusId, setPendingPersonaFocusId] = useState<string | null>(null);
  const personaCardInputRef = useRef<HTMLInputElement | null>(null);

  const filteredPersonas = useMemo(() => {
    const keyword = search.trim().toLowerCase();
    return personaList.filter((persona) => {
      const matchesKeyword = [persona.name, persona.id, persona.summary, persona.author]
        .join(' ')
        .toLowerCase()
        .includes(keyword);
      const matchesStatus =
        statusFilter === 'all' ||
        (statusFilter === 'active' && persona.id === activePersonaId) ||
        (statusFilter === 'inactive' && persona.id !== activePersonaId);
      return matchesKeyword && matchesStatus;
    });
  }, [activePersonaId, personaList, search, statusFilter]);

  function applyPersonaList(personas: PersonaLibraryItem[], currentId: string | null) {
    setPersonaList(personas);
    setActivePersonaId(currentId);
  }

  function applyActivePersona(payload: ActivePersonaResponse) {
    setActivePersona(payload.persona);
    setActiveVisualPack(payload.visual_pack);
    setActivePersonaId(payload.persona.id);
  }

  function clearActivePersona() {
    setActivePersona(null);
    setActiveVisualPack(null);
  }

  function closeEditor() {
    setEditor((state) => ({ ...state, open: false }));
  }

  function openImportDialog() {
    setImportDialogOpen(true);
  }

  function closeImportDialog() {
    setImportDialogOpen(false);
  }

  function resetImportDraft() {
    setImportText('');
    setImportFileName('');
    setImportConflictStrategy('rename');
    setActivateAfterImport(true);
  }

  function requestPersonaFocus(id: string) {
    setPendingPersonaFocusId(id);
  }

  function clearPersonaFocusRequest() {
    setPendingPersonaFocusId(null);
  }

  function updateEditor<K extends keyof Persona>(key: K, value: Persona[K]) {
    setEditor((state) => ({
      ...state,
      persona: {
        ...state.persona,
        [key]: value
      }
    }));
  }

  function updateEditorVisualDraft<K extends keyof PersonaVisualPackPatch>(
    key: K,
    value: PersonaVisualPackPatch[K]
  ) {
    setEditor((state) => ({
      ...state,
      visualPackDraft: {
        ...state.visualPackDraft,
        [key]: value
      },
      visualDirty: true
    }));
  }

  function updateEditorToolPolicyMode(mode: Persona['tool_policy']['mode']) {
    setEditor((state) => ({
      ...state,
      persona: {
        ...state.persona,
        tool_policy: {
          ...state.persona.tool_policy,
          mode
        }
      }
    }));
  }

  function updateEditorAllowedTools(value: string) {
    const allowedTools = value
      .split(',')
      .map((item) => item.trim())
      .filter(Boolean);
    setEditor((state) => ({
      ...state,
      persona: {
        ...state.persona,
        tool_policy: {
          ...state.persona.tool_policy,
          allowed_tools: allowedTools
        }
      }
    }));
  }

  return {
    personaList,
    activePersonaId,
    activePersona,
    activeVisualPack,
    search,
    setSearch,
    statusFilter,
    setStatusFilter,
    filteredPersonas,
    editor,
    setEditor,
    closeEditor,
    importDialogOpen,
    openImportDialog,
    closeImportDialog,
    importText,
    setImportText,
    importFileName,
    setImportFileName,
    importConflictStrategy,
    setImportConflictStrategy,
    activateAfterImport,
    setActivateAfterImport,
    personaCardInputRef,
    resetImportDraft,
    pendingPersonaFocusId,
    requestPersonaFocus,
    clearPersonaFocusRequest,
    applyPersonaList,
    applyActivePersona,
    clearActivePersona,
    updateEditor,
    updateEditorVisualDraft,
    updateEditorToolPolicyMode,
    updateEditorAllowedTools
  };
}
