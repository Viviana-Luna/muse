import { useState } from 'react';
import type { ChangeEvent } from 'react';

import {
  activatePersona,
  deletePersona,
  exportPersonaCard,
  fetchActivePersona,
  fetchPersona,
  fetchPersonas,
  importPersonaCard,
  savePersona
} from '@/api';
import { ApiError } from '@/api/client';
import type { AppToastInput } from '@/hooks/useAppToast';
import type {
  ActivePersonaResponse,
  Persona,
  PersonaCard,
  PersonaMutationResponse,
  PersonaVisualPackPatch,
  RuntimeStateResponse
} from '@/types';
import type {
  OperationState,
  RevisionBoundActivePersona,
  StoryPersonaSnapshot,
  StoryRuntimeSnapshot
} from '@/views/story/types';
import {
  createPersonaVisualDraft,
  DEFAULT_PERSONA,
  type PersonaEditorMode,
  type UsePersonaStateResult
} from './usePersonaState';

interface UsePersonaControllerOptions {
  state: UsePersonaStateResult;
  notify: (input: AppToastInput) => void;
  getMutationBlockReason?: () => string | null;
  setBusy: (busy: boolean) => void;
  setRuntimeStatus: (status: string) => void;
  acceptStateRevision: (revision: number) => boolean;
  loadStableRuntimeState: <T>(
    load: (stableState: RuntimeStateResponse) => Promise<T>
  ) => Promise<{ value: T; state: RuntimeStateResponse }>;
  showActivePersona: (active: ActivePersonaResponse) => void;
  synchronizeRuntimeAfterPersonaReset: (
    candidateActive: RevisionBoundActivePersona
  ) => Promise<StoryRuntimeSnapshot>;
  closeCurrentChatSource: () => void;
  stopVoice: () => void;
  navigateToStory: () => void;
  onPersonaRefreshStart?: () => void;
  onPersonaRefreshSuccess?: (snapshot: StoryPersonaSnapshot) => void;
  onPersonaRefreshError?: (error: unknown) => void;
}

/**
 * 角色领域控制器。
 *
 * 所有 mutation 都以后端返回的 active persona、conversation 与 revision 为事实源；
 * 导入但不启用时不会把导入对象写成当前角色。
 */
export function usePersonaController(options: UsePersonaControllerOptions) {
  const [personaImportError, setPersonaImportError] = useState('');
  const [transitionState, setTransitionState] = useState<OperationState>({ status: 'idle' });
  const { state } = options;

  function rejectBlockedMutation(action: string): boolean {
    const reason = options.getMutationBlockReason?.();
    if (!reason) return false;
    options.setRuntimeStatus(reason);
    options.notify({
      title: `${action}暂不可用`,
      description: reason,
      tone: 'info'
    });
    return true;
  }

  function applyActive(payload: ActivePersonaResponse) {
    state.applyActivePersona(payload);
    options.showActivePersona(payload);
  }

  function candidateFromMutation(
    payload: Pick<
      PersonaMutationResponse,
      'active_persona' | 'active_persona_id' | 'visual_pack' | 'state_revision'
    >
  ): RevisionBoundActivePersona {
    if (payload.active_persona_id === null) {
      if (payload.active_persona !== null || payload.visual_pack !== null) {
        throw new Error('角色 mutation 响应中的空角色事实不一致。');
      }
      return { stateRevision: payload.state_revision, active: null };
    }
    if (!payload.active_persona || payload.active_persona_id !== payload.active_persona.id) {
      throw new Error('角色 mutation 响应缺少同源的活动角色与展示资源。');
    }
    return {
      stateRevision: payload.state_revision,
      active: {
        persona: payload.active_persona,
        visual_pack: payload.visual_pack
      }
    };
  }

  function commitPersonaSnapshot(snapshot: StoryPersonaSnapshot) {
    if (!options.acceptStateRevision(snapshot.stateRevision)) {
      throw new Error('角色快照在提交前已被更新的运行时事实取代。');
    }
    state.applyPersonaList(snapshot.personas.personas, snapshot.activePersonaId);
    if (snapshot.active) {
      applyActive(snapshot.active);
    } else {
      state.clearActivePersona();
    }
    options.onPersonaRefreshSuccess?.(snapshot);
  }

  async function refreshPersonas(
    candidateActive?: RevisionBoundActivePersona
  ): Promise<StoryPersonaSnapshot> {
    options.onPersonaRefreshStart?.();
    try {
      const { value, state: runtimeState } = await options.loadStableRuntimeState(async (stableState) => {
        const personas = await fetchPersonas();
        const candidateId = candidateActive?.active?.persona.id ?? null;
        if (
          candidateActive !== undefined &&
          candidateActive.stateRevision === stableState.state_revision &&
          candidateId === stableState.active_persona_id &&
          personas.active_persona_id === stableState.active_persona_id
        ) {
          return { personas, active: candidateActive.active, activeState: null };
        }
        const activeState = await fetchActivePersona();
        return { personas, active: null, activeState };
      });
      let active = value.active;
      if (value.activeState) {
        if (value.activeState.state_revision !== runtimeState.state_revision) {
          throw new Error('活动角色响应不属于当前稳定 revision。');
        }
        if (runtimeState.active_persona_id === null) {
          if (
            value.activeState.active_persona_id !== null ||
            value.activeState.active_persona !== null ||
            value.activeState.visual_pack !== null
          ) {
            throw new Error('活动角色接口与运行时空角色事实不一致。');
          }
          active = null;
        } else {
          if (
            value.activeState.active_persona_id !== runtimeState.active_persona_id ||
            value.activeState.active_persona?.id !== runtimeState.active_persona_id
          ) {
            throw new Error('活动角色接口与运行时角色事实不一致。');
          }
          active = {
            persona: value.activeState.active_persona,
            visual_pack: value.activeState.visual_pack
          };
        }
      }
      if (
        value.personas.active_persona_id !== runtimeState.active_persona_id ||
        (runtimeState.active_persona_id === null && active !== null) ||
        (runtimeState.active_persona_id !== null &&
          active?.persona.id !== runtimeState.active_persona_id)
      ) {
        throw new Error('角色列表、活动角色与运行时 revision 无法组成同一事实快照。');
      }
      const snapshot = {
        stateRevision: runtimeState.state_revision,
        personas: value.personas,
        activePersonaId: runtimeState.active_persona_id,
        active
      };
      commitPersonaSnapshot(snapshot);
      return snapshot;
    } catch (error) {
      options.onPersonaRefreshError?.(error);
      throw error;
    }
  }

  async function synchronizeAndCommit(
    candidateActive: RevisionBoundActivePersona,
    runtimeReset: boolean
  ): Promise<StoryPersonaSnapshot> {
    if (!runtimeReset) return refreshPersonas(candidateActive);
    try {
      options.onPersonaRefreshStart?.();
      return await options.synchronizeRuntimeAfterPersonaReset(candidateActive);
    } catch (error) {
      options.onPersonaRefreshError?.(error);
      throw error;
    }
  }

  function stopInteractiveRuntime() {
    options.closeCurrentChatSource();
    options.stopVoice();
  }

  async function handleActivate(id: string) {
    if (rejectBlockedMutation('切换角色')) return;
    setTransitionState({ status: 'pending' });
    options.setBusy(true);
    try {
      stopInteractiveRuntime();
      const payload = await activatePersona(id);
      options.acceptStateRevision(payload.state_revision);
      const snapshot = await synchronizeAndCommit(candidateFromMutation(payload), true);
      options.navigateToStory();
      options.setRuntimeStatus(`已切换到角色：${snapshot.active?.persona.name || '未选择角色'}`);
      setTransitionState({ status: 'succeeded' });
    } catch (err) {
      const message = err instanceof Error ? err.message : '无法切换到所选角色。';
      setTransitionState({ status: 'failed', error: message });
      options.notify({
        title: '切换角色失败',
        description: message,
        tone: 'error'
      });
    } finally {
      options.setBusy(false);
    }
  }

  async function openEditor(mode: PersonaEditorMode, summary?: { id: string }) {
    if (mode === 'create') {
      state.setEditor({
        open: true,
        mode,
        persona: {
          ...DEFAULT_PERSONA,
          id: `persona-${Date.now()}`,
          name: '新角色'
        },
        visualPackDraft: createPersonaVisualDraft(null),
        visualDirty: false
      });
      return;
    }
    if (!summary) return;
    const detail = await fetchPersona(summary.id);
    const source = detail.persona;
    state.setEditor({
      open: true,
      mode,
      persona:
        mode === 'copy'
          ? {
              ...source,
              id: `${source.id}-copy-${Date.now()}`,
              name: `${source.name} 副本`
            }
          : source,
      visualPackDraft: createPersonaVisualDraft(detail.visual_pack),
      visualDirty: false
    });
  }

  async function saveEditorDraft(
    persona: Persona,
    mode: PersonaEditorMode,
    visualPack?: PersonaVisualPackPatch
  ): Promise<boolean> {
    if (rejectBlockedMutation('保存角色')) return false;
    setTransitionState({ status: 'pending' });
    options.setBusy(true);
    try {
      const editing = mode === 'edit';
      const activateAfterCreate = mode === 'create' && state.personaList.length === 0;
      stopInteractiveRuntime();
      const payload = await savePersona(persona, editing, visualPack, {
        activateAfterCreate
      });
      options.acceptStateRevision(payload.state_revision);
      const candidate = candidateFromMutation(payload);
      if (
        activateAfterCreate &&
        (candidate.active?.persona.id !== payload.affected_persona.id || !payload.runtime_reset)
      ) {
        await synchronizeAndCommit(candidate, payload.runtime_reset);
        state.setEditor((current) => ({
          ...current,
          mode: 'edit',
          persona: payload.affected_persona
        }));
        options.setRuntimeStatus('角色已创建，但后端未完成原子启用。');
        throw new Error('角色已创建，但后端未返回“创建并启用”的完整事实结果。');
      }
      await synchronizeAndCommit(candidate, payload.runtime_reset);
      options.setRuntimeStatus(
        editing
          ? '角色已更新。'
          : activateAfterCreate
            ? '角色已创建并启用。'
            : '角色已创建。'
      );
      setTransitionState({ status: 'succeeded' });
      return true;
    } catch (error) {
      setTransitionState({
        status: 'failed',
        error: error instanceof Error ? error.message : '保存角色失败。'
      });
      throw error;
    } finally {
      options.setBusy(false);
    }
  }

  async function handleDelete(id: string) {
    if (rejectBlockedMutation('删除角色')) return;
    setTransitionState({ status: 'pending' });
    options.setBusy(true);
    try {
      const deletingActive = id === state.activePersonaId;
      stopInteractiveRuntime();
      const payload = await deletePersona(id);
      options.acceptStateRevision(payload.state_revision);
      await synchronizeAndCommit(
        candidateFromMutation(payload),
        payload.runtime_reset || deletingActive
      );
      options.setRuntimeStatus('角色已删除。');
      options.notify({ title: '角色已删除', tone: 'success' });
      setTransitionState({ status: 'succeeded' });
    } catch (err) {
      const message = err instanceof Error ? err.message : '无法删除所选角色。';
      setTransitionState({ status: 'failed', error: message });
      options.notify({
        title: '删除角色失败',
        description: message,
        tone: 'error'
      });
    } finally {
      options.setBusy(false);
    }
  }

  async function handleExport(
    id: string,
    level: PersonaCard['export_level'] = 'with_visual_pack_ref'
  ) {
    const card = await exportPersonaCard(id, level);
    const blob = new Blob([JSON.stringify(card, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `${card.persona.id}.${level === 'persona_only' ? 'light' : 'full'}.muse-role-card.json`;
    link.click();
    URL.revokeObjectURL(url);
  }

  async function handleImport() {
    if (!state.importText.trim()) return;
    if (rejectBlockedMutation('导入角色')) return;
    setPersonaImportError('');
    setTransitionState({ status: 'pending' });
    options.setBusy(true);
    try {
      const card = JSON.parse(state.importText) as PersonaCard;
      stopInteractiveRuntime();
      const payload = await importPersonaCard(card, {
        conflictStrategy: state.importConflictStrategy,
        activateAfterImport: state.activateAfterImport
      });
      options.acceptStateRevision(payload.state_revision);
      await synchronizeAndCommit(candidateFromMutation(payload), payload.runtime_reset);
      state.requestPersonaFocus(payload.affected_persona.id);
      state.resetImportDraft();
      state.closeImportDialog();
      options.setRuntimeStatus(
        payload.notices.length ? payload.notices.join('；') : '角色卡片已导入。'
      );
      options.notify({
        title: '角色卡片已导入',
        description: payload.notices.join('；') || undefined,
        tone: 'success'
      });
      setTransitionState({ status: 'succeeded' });
    } catch (err) {
      const importError =
        err instanceof SyntaxError
          ? '角色卡不是有效的 JSON 文件。'
          : err instanceof ApiError && Object.keys(err.fieldErrors).length > 0
            ? Object.values(err.fieldErrors)[0]
            : err instanceof Error
              ? err.message
              : '无法导入当前角色卡。';
      setPersonaImportError(importError);
      setTransitionState({ status: 'failed', error: importError });
      options.notify({
        title: '导入角色卡失败',
        description: importError,
        tone: 'error'
      });
    } finally {
      options.setBusy(false);
    }
  }

  async function handlePersonaCardFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = '';
    if (!file) return;
    try {
      const importText = await file.text();
      state.openImportDialog();
      state.setImportFileName(file.name);
      state.setImportText(importText);
      setPersonaImportError('');
      options.notify({
        title: '已读取角色卡',
        description: `文件：${file.name}`,
        tone: 'success'
      });
    } catch (err) {
      options.notify({
        title: '读取角色卡失败',
        description: err instanceof Error ? err.message : '无法读取所选文件。',
        tone: 'error'
      });
    }
  }

  return {
    personaImportError,
    setPersonaImportError,
    transitionState,
    refreshPersonas,
    handleActivate,
    openEditor,
    saveEditorDraft,
    handleDelete,
    handleExport,
    handleImport,
    handlePersonaCardFile
  };
}
