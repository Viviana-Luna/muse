// 主应用视图，整合聊天舞台、角色管理、模型设置和运行时工具事件。

import { lazy, Suspense, useEffect, useMemo, useState } from 'react';
import { Check } from 'lucide-react';
import { reportDesktopReady } from '@/api/client';
import {
  migrateLegacyMuseUrl,
  readMuseRoute,
  writeMuseRoute,
  type MuseRoute,
  type MuseSection
} from '@/app/routes';
import { AppRail } from '@/components/shell/AppRail';
import { AppTitleBar } from '@/components/shell/AppTitleBar';
import { AppToastViewport } from '@/components/notification/AppToastViewport';
import { RuntimeLiveRegion } from '@/components/feedback/RuntimeLiveRegion';
import { LazySurfaceBoundary } from '@/components/feedback/LazySurfaceBoundary';
import { AppLoadingScreen, SectionLoading } from '@/components/feedback/LoadingState';
import { useAppToast } from '@/hooks/useAppToast';
import { useModalAccessibility } from '@/hooks/useModalAccessibility';
import { usePersonaTheme } from '@/hooks/usePersonaTheme';
import { usePersonaState } from '@/views/personas/hooks/usePersonaState';
import { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import { PersonaLibraryView } from '@/views/personas/library/PersonaLibraryView';
import { beginPersonaImport } from '@/views/personas/personaImportFlow';
import { useSettingsDraft } from '@/views/hooks/useSettingsDraft';
import { useSettingsController } from '@/views/hooks/useSettingsController';
import { useChatRuntime } from '@/views/chat/hooks/useChatRuntime';
import { type SettingsPanel } from '@/views/settings/types';
import { StoryWorkspace } from '@/views/story/components/StoryWorkspace';
import { useStoryWorkspaceState } from '@/views/story/hooks/useStoryWorkspaceState';
import { sessionMeta, sessionTitle } from '@/views/chat/components/HistoryRail';
import {
  calculateDeepSeekUsdCost,
  formatExactTokenCount,
  formatTokenCount,
  formatUsdCost
} from '@/views/chat/tokenActivity';
import { SessionsPage } from '@/views/sessions/SessionsPage';
import { SkillsPage } from '@/views/skills/SkillsPage';
import { McpPage } from '@/views/mcp/McpPage';

const loadSettingsDialog = () =>
  import('@/views/settings/SettingsDialog').then((module) => ({ default: module.SettingsDialog }));
const loadPersonaEditorDialog = () =>
  import('@/views/personas/PersonaEditorDialog').then((module) => ({
    default: module.PersonaEditorDialog
  }));

// 应用主组件。
export function App() {
  const { toasts, notify, dismissToast } = useAppToast();
  const [pendingImportIntent, setPendingImportIntent] = useState(false);
  const [railCollapsed, setRailCollapsed] = useState(false);
  const [personaSelectorOpen, setPersonaSelectorOpen] = useState(false);
  const [route, setRoute] = useState<MuseRoute>(() => {
    migrateLegacyMuseUrl();
    return readMuseRoute();
  });
  const [settingsChunkAttempt, setSettingsChunkAttempt] = useState(0);
  const [personaEditorChunkAttempt, setPersonaEditorChunkAttempt] = useState(0);
  const SettingsDialog = useMemo(() => lazy(loadSettingsDialog), [settingsChunkAttempt]);
  const PersonaEditorDialog = useMemo(
    () => lazy(loadPersonaEditorDialog),
    [personaEditorChunkAttempt]
  );
  const personaState = usePersonaState();
  const storyState = useStoryWorkspaceState();
  const {
    personaList,
    activePersonaId,
    activePersona,
    activeVisualPack,
    editor,
    setEditor,
    closeEditor,
    openImportDialog,
    applyPersonaList,
    applyActivePersona,
    clearActivePersona,
    updateEditor,
    updateEditorVisualDraft,
    updateEditorToolPolicyMode,
    updateEditorAllowedTools
  } = personaState;
  const settingsDraft = useSettingsDraft();
  const {
    settingsOpen,
    setSettingsOpen,
    settingsPanel,
    setSettingsPanel,
    settingsConfig,
    setSettingsConfig,
    workspacePermissionMode,
    workspaceSandboxMode,
    webSearchConfigured,
    webSearchKeyDraft,
    stageWebSearchKey,
    stageWebSearchDelete,
    webSearchAction,
    appearanceSettings,
    setAppearanceSettings,
    modelCatalog,
    setModelCatalog,
    diagnosticsChecks,
    setDiagnosticsChecks,
    modelPicker,
    setModelPicker,
    modelFetchStatus,
    setModelFetchStatus,
    maskedKeys,
    setMaskedKeys,
    applyLoadedSettings,
    applyWebSearchState,
    dirtyDomains,
    markModelsSaved,
    markWorkspaceSaved,
    markAppearanceSaved,
    stageWorkspacePolicy,
    discardSettingsChanges,
    updateSection,
    updateSpeechRecognition
  } = settingsDraft;
  const modelPickerModalRef = useModalAccessibility<HTMLElement>(
    Boolean(modelPicker && !modelPicker.commitOnSelect),
    () => setModelPicker(null)
  );
  const chatRuntime = useChatRuntime({
    activePersona,
    notify,
    getMutationBlockReason: () => storyState.chatMutationBlockReason,
    onPersonaBootstrapStart: storyState.beginLoad,
    onPersonaBootstrap: (snapshot) => {
      applyPersonaList(snapshot.personas.personas, snapshot.activePersonaId);
      if (snapshot.active) applyActivePersona(snapshot.active);
      else clearActivePersona();
      storyState.commitSnapshot(snapshot);
    },
    onPersonaBootstrapError: storyState.failLoad
  });
  const {
    modelLabel,
    setModelLabel,
    busy,
    setBusy,
    runtimeStatus,
    setRuntimeStatus,
    closeCurrentChatSource,
    stopVoice,
    voice,
    speak,
    refreshVoiceCapabilities,
    acceptStateRevision,
    loadStableRuntimeState,
    showActivePersona,
    synchronizeRuntimeAfterPersonaReset
  } = chatRuntime;
  const personaController = usePersonaController({
    state: personaState,
    notify,
    getMutationBlockReason: () => storyState.mutationBlockReason,
    setBusy,
    setRuntimeStatus,
    acceptStateRevision,
    loadStableRuntimeState,
    showActivePersona,
    synchronizeRuntimeAfterPersonaReset,
    closeCurrentChatSource,
    stopVoice,
    navigateToStory: () => navigateMuseSection('chat'),
    onPersonaRefreshStart: storyState.beginLoad,
    onPersonaRefreshSuccess: storyState.commitSnapshot,
    onPersonaRefreshError: storyState.failLoad
  });
  const { transitionState, handleActivate, openEditor, saveEditorDraft } = personaController;
  const {
    initializeAppearancePreferences,
    openSettings,
    submitSettings,
    handleDeleteWebSearchKey,
    handleUpdateWorkspacePolicy,
    handlePreviewTts,
    handleRefreshDiagnostics,
    saveCatalogProviderCredential,
    deleteCatalogProviderCredential,
    verifyCatalogProvider,
    createManagedModel,
    updateManagedModel,
    deleteManagedModel,
    loadProviderModels,
    checkProviderBalance,
    openRuntimeModelPicker,
    chooseFetchedModel,
    updateProvider,
    providersForPurpose,
    providerAllowsCustomBase,
    providerSupportsBalance,
    selectedCatalogModel,
    capabilityBadges,
  } = useSettingsController({
    draft: settingsDraft,
    notify,
    setBusy,
    speak,
    refreshVoiceCapabilities,
    setModelLabel,
    navigateToStory: () => navigateMuseSection('chat')
  });

  function navigateMuseSection(section: MuseSection, panel?: SettingsPanel, name?: string) {
    if (panel) setSettingsPanel(panel);
    const next = { section, name } satisfies MuseRoute;
    setRoute(next);
    writeMuseRoute(next);
  }

  useEffect(() => {
    void reportDesktopReady().catch(() => {
      // 就绪失败会由现有运行时状态面板展示；这里避免输出 Bootstrap 或令牌细节。
      console.error('桌面内嵌页未能完成本地运行时就绪校验。');
    });
  }, []);

  useEffect(() => {
    void initializeAppearancePreferences();
  }, []);

  useEffect(() => {
    const syncViewFromUrl = () => setRoute(readMuseRoute());
    window.addEventListener('hashchange', syncViewFromUrl);
    window.addEventListener('popstate', syncViewFromUrl);
    return () => {
      window.removeEventListener('hashchange', syncViewFromUrl);
      window.removeEventListener('popstate', syncViewFromUrl);
    };
  }, []);

  useEffect(() => {
    if (route.section === 'settings') {
      if (!settingsOpen) void openSettings(settingsPanel);
      return;
    }
    setSettingsOpen(false);
  }, [route.section]);

  useEffect(() => {
    if (route.section !== 'roles' || !pendingImportIntent) return;
    setPendingImportIntent(false);
    openImportDialog();
  }, [route.section, openImportDialog, pendingImportIntent]);


  const personaTheme = usePersonaTheme(activeVisualPack, voice.status, appearanceSettings);
  const { themeMode, rootStyle } = personaTheme;
  const selectableSessions = chatRuntime.runtimeSessions.filter(
    (session) => session.can_resume && session.records > 0
  );
  const selectedTitlebarSession =
    chatRuntime.selectedRuntimeSession ??
    chatRuntime.runtimeSessions.find(
      (session) => session.conversation_id === chatRuntime.selectedConversationId
    );
  const activeTitlebarSession = chatRuntime.runtimeSessions.find(
    (session) => session.conversation_id === chatRuntime.activeConversationId
  );
  const activeConversationTitle = activeTitlebarSession
    ? sessionTitle(activeTitlebarSession)
    : activePersona
      ? `与${activePersona.name}的对话`
      : '新对话';
  const conversationTitle = selectedTitlebarSession
    ? sessionTitle(selectedTitlebarSession)
    : activeConversationTitle;
  const runtimeContextSnapshot = chatRuntime.runtimeContextSnapshot;
  const contextProgress = runtimeContextSnapshot
    ? Math.max(0, Math.min(100, runtimeContextSnapshot.usage_percent))
    : null;
  const currentProviderLabel = modelLabel.split(' / ')[0]?.trim() ?? '';
  const showDeepSeekCost = /^deepseek$/i.test(currentProviderLabel);
  const deepSeekCost = showDeepSeekCost
    ? calculateDeepSeekUsdCost(chatRuntime.runtimeTokenUsage?.items ?? [])
    : null;
  const activeSessionNeedsFallback = !selectableSessions.some(
    (session) => session.conversation_id === chatRuntime.activeConversationId
  );
  const appLoading =
    storyState.resourceState.status === 'initial' ||
    storyState.resourceState.status === 'loading';

  if (appLoading) return <AppLoadingScreen />;

  return (
    <>
      <AppToastViewport toasts={toasts} themeMode={themeMode} onDismiss={dismissToast} />
      <main
        className={`runtime-shell story-shell app-shell view-${route.section} theme-${themeMode} motion-${appearanceSettings.motionLevel}${railCollapsed ? ' rail-collapsed' : ''}`}
        style={rootStyle}
      >
      <RuntimeLiveRegion status={runtimeStatus} />
      <AppTitleBar
        railCollapsed={railCollapsed}
        activePersonaName={activePersona?.name}
        modelLabel={modelLabel}
        conversationContext={{
          title: conversationTitle,
          stateLabel: chatRuntime.selectedConversationReadOnly ? '历史会话' : '当前会话',
          contextProgress,
          usageLabel: contextProgress === null ? '--' : `${Math.round(contextProgress)}%`,
          tokenLabel: runtimeContextSnapshot
            ? formatExactTokenCount(runtimeContextSnapshot.used_total_tokens)
            : '--',
          detailLabel: runtimeContextSnapshot
            ? `${formatTokenCount(runtimeContextSnapshot.used_total_tokens)} / ${formatTokenCount(runtimeContextSnapshot.context_window)}，剩余 ${formatTokenCount(runtimeContextSnapshot.remaining_tokens)}`
            : '等待上下文快照',
          costLabel: showDeepSeekCost ? formatUsdCost(deepSeekCost) : undefined
        }}
        sessionSelectorOpen={chatRuntime.sessionPanelOpen}
        sessionSelectorDisabled={busy}
        sessionPickerContent={
          chatRuntime.sessionPanelOpen ? (
            <section className="titlebar-session-popover" aria-label="聊天记录选择器">
              <header>
                <span>
                  <small>聊天记录</small>
                  <strong>{selectableSessions.length} 个已保存会话</strong>
                </span>
                <button
                  type="button"
                  onClick={() => {
                    chatRuntime.setSessionPanelOpen(false);
                    navigateMuseSection('sessions');
                  }}
                >
                  管理全部
                </button>
              </header>
              <div className="titlebar-session-popover-list" role="listbox" aria-label="聊天记录">
                {activeSessionNeedsFallback && (
                  <button
                    type="button"
                    role="option"
                    aria-selected={
                      chatRuntime.selectedConversationId === chatRuntime.activeConversationId
                    }
                    disabled={busy}
                    onClick={() =>
                      void chatRuntime.selectSessionFromHistory(chatRuntime.activeConversationId)
                    }
                  >
                    <span>
                      <strong>{activeConversationTitle}</strong>
                      <small>尚无已保存记录</small>
                    </span>
                    <em>当前</em>
                    {chatRuntime.selectedConversationId === chatRuntime.activeConversationId && (
                      <Check aria-hidden="true" />
                    )}
                  </button>
                )}
                {selectableSessions.map((session) => {
                  const selected =
                    session.conversation_id === chatRuntime.selectedConversationId;
                  const current = session.conversation_id === chatRuntime.activeConversationId;
                  return (
                    <button
                      type="button"
                      role="option"
                      aria-selected={selected}
                      key={session.conversation_id}
                      disabled={busy}
                      onClick={() =>
                        void chatRuntime.selectSessionFromHistory(session.conversation_id)
                      }
                    >
                      <span>
                        <strong>{sessionTitle(session)}</strong>
                        <small>{sessionMeta(session)}</small>
                      </span>
                      {(current || session.archived) && (
                        <em>{current ? '当前' : '归档'}</em>
                      )}
                      {selected && <Check aria-hidden="true" />}
                    </button>
                  );
                })}
              </div>
            </section>
          ) : undefined
        }
        personaSelectorOpen={personaSelectorOpen}
        personaSelectorDisabled={busy || personaList.length === 0}
        personaPickerContent={
          personaSelectorOpen ? (
            <section
              className="titlebar-model-popover titlebar-persona-popover"
              aria-label="选择当前角色"
            >
              <div
                className="titlebar-model-popover-list titlebar-persona-popover-list"
                role="listbox"
                aria-label="角色"
              >
                {personaList.map((persona) => {
                  const current = persona.id === activePersonaId;
                  return (
                    <button
                      type="button"
                      role="option"
                      aria-selected={current}
                      key={persona.id}
                      disabled={busy}
                      onClick={() => {
                        setPersonaSelectorOpen(false);
                        if (!current) void handleActivate(persona.id);
                      }}
                    >
                      <span className="titlebar-persona-avatar" aria-hidden="true">
                        {persona.name.trim().slice(0, 1) || '角'}
                      </span>
                      <span>
                        <strong>{persona.name}</strong>
                        <small>{persona.summary || '暂无角色简介'}</small>
                      </span>
                      {current && <Check aria-hidden="true" />}
                    </button>
                  );
                })}
              </div>
            </section>
          ) : undefined
        }
        modelSelectorOpen={Boolean(modelPicker?.commitOnSelect)}
        modelSelectorDisabled={busy}
        modelPickerContent={
          modelPicker?.commitOnSelect ? (
            <section className="titlebar-model-popover" aria-label="选择聊天模型">
              <div className="titlebar-model-popover-list" role="listbox" aria-label="聊天模型">
                {modelPicker.models.map((model) => {
                  const providerName =
                    modelCatalog?.providers.find((provider) => provider.id === model.provider_id)
                      ?.name ?? model.provider_id;
                  const current =
                    model.provider_id === modelPicker.currentProvider &&
                    model.model === modelPicker.currentModel;
                  return (
                    <button
                      type="button"
                      role="option"
                      aria-selected={current}
                      key={model.id}
                      disabled={busy}
                      onClick={() => void chooseFetchedModel(modelPicker.purpose, model)}
                    >
                      <span>
                        <strong>{model.name}</strong>
                        <small>{providerName} · {model.model}</small>
                      </span>
                      {current && <Check aria-hidden="true" />}
                    </button>
                  );
                })}
              </div>
            </section>
          ) : undefined
        }
        onToggleRail={() => setRailCollapsed((collapsed) => !collapsed)}
        onOpenSessionSelector={() => {
          setPersonaSelectorOpen(false);
          setModelPicker(null);
          chatRuntime.setSessionPanelOpen((open) => !open);
        }}
        onCloseSessionSelector={() => chatRuntime.setSessionPanelOpen(false)}
        onOpenPersonaSelector={() => {
          chatRuntime.setSessionPanelOpen(false);
          setModelPicker(null);
          setPersonaSelectorOpen((open) => !open);
        }}
        onClosePersonaSelector={() => setPersonaSelectorOpen(false)}
        onOpenModelSelector={() => {
          setPersonaSelectorOpen(false);
          chatRuntime.setSessionPanelOpen(false);
          if (modelPicker?.commitOnSelect) setModelPicker(null);
          else void openRuntimeModelPicker();
        }}
        onCloseModelSelector={() => setModelPicker(null)}
      />
      <AppRail active={route.section} onNavigate={(section) => navigateMuseSection(section)} />
      <section className="app-main-surface">
      {route.section === 'chat' && (
        <StoryWorkspace
          resourceState={storyState.resourceState}
          transitionState={transitionState}
          runtime={chatRuntime}
          theme={personaTheme}
          onCreatePersona={() => void openEditor('create')}
          onImportPersona={() =>
            beginPersonaImport({
              markPending: () => setPendingImportIntent(true),
              openLibrary: () => navigateMuseSection('roles')
            })
          }
          onOpenPersonaLibrary={() => navigateMuseSection('roles')}
          onOpenDiagnostics={() => navigateMuseSection('settings', 'diagnostics')}
          onOpenSessions={() => navigateMuseSection('sessions')}
        />
      )}

      {route.section === 'sessions' && (
        <SessionsPage
          runtime={chatRuntime}
          activePersonaName={activePersona?.name}
          onOpenChat={() => navigateMuseSection('chat')}
        />
      )}

      {route.section === 'roles' && (
        <PersonaLibraryView
          state={personaState}
          controller={personaController}
          resourceState={storyState.resourceState}
          busy={busy || Boolean(storyState.mutationBlockReason)}
          onClose={() => navigateMuseSection('chat')}
        />
      )}

      {route.section === 'skills' && (
        <SkillsPage
          selectedName={route.name}
          onSelectedNameChange={(name) => navigateMuseSection('skills', undefined, name)}
          notify={notify}
        />
      )}
      {route.section === 'mcp' && (
        <McpPage
          selectedName={route.name}
          onSelectedNameChange={(name) => navigateMuseSection('mcp', undefined, name)}
          notify={notify}
        />
      )}

      {route.section === 'settings' && (!settingsOpen || !settingsConfig) && (
        <SectionLoading
          label="正在加载设置中心"
          description="正在读取本地设置…"
        />
      )}

      {editor.open && (
        <LazySurfaceBoundary
          label="角色编辑器"
          onRetry={() => setPersonaEditorChunkAttempt((attempt) => attempt + 1)}
          onClose={closeEditor}
        >
          <Suspense
            fallback={
              <section className="modal-shell" aria-label="角色编辑器加载表面">
                <SectionLoading
                  label="正在加载角色编辑器"
                  description="正在准备角色表单…"
                  variant="surface"
                />
              </section>
            }
          >
            <PersonaEditorDialog
              editor={editor}
              busy={busy || Boolean(storyState.mutationBlockReason)}
              notify={notify}
              onClose={closeEditor}
              onSave={saveEditorDraft}
              setEditor={setEditor}
              updateEditor={updateEditor}
              updateEditorVisualDraft={updateEditorVisualDraft}
              updateEditorToolPolicyMode={updateEditorToolPolicyMode}
              updateEditorAllowedTools={updateEditorAllowedTools}
              primaryActionLabel={
                editor.mode === 'create' && personaState.personaList.length === 0
                  ? '创建并启用'
                  : undefined
              }
            />
          </Suspense>
        </LazySurfaceBoundary>
      )}

      {route.section === 'settings' && settingsOpen && settingsConfig && (
        <LazySurfaceBoundary
          label="设置中心"
          onRetry={() => setSettingsChunkAttempt((attempt) => attempt + 1)}
          onClose={() => navigateMuseSection('chat')}
        >
          <Suspense
            fallback={
              <section className="modal-shell" aria-label="设置中心加载表面">
                <SectionLoading
                  label="正在加载设置中心"
                  description="正在准备设置界面…"
                  variant="surface"
                />
              </section>
            }
          >
            <SettingsDialog
            embedded
            settingsConfig={settingsConfig}
            modelCatalog={modelCatalog}
            settingsPanel={settingsPanel}
            busy={busy}
            maskedKeys={maskedKeys}
            modelFetchStatus={modelFetchStatus}
            workspacePermissionMode={workspacePermissionMode}
            workspaceSandboxMode={workspaceSandboxMode}
            webSearchConfigured={webSearchConfigured}
            webSearchKeyDraft={webSearchKeyDraft}
            webSearchAction={webSearchAction}
            dirtyDomains={dirtyDomains}
            runtimeStatus={runtimeStatus}
            voiceStatus={voice.status}
            diagnosticsChecks={diagnosticsChecks}
            appearanceSettings={appearanceSettings}
            setSettingsPanel={setSettingsPanel}
            setSettingsConfig={setSettingsConfig}
            setModelCatalog={setModelCatalog}
            setAppearanceSettings={setAppearanceSettings}
            setWebSearchKeyDraft={stageWebSearchKey}
            onClose={() => navigateMuseSection('chat')}
            onDiscard={discardSettingsChanges}
            onSubmit={submitSettings}
            onUpdateWorkspacePolicy={handleUpdateWorkspacePolicy}
            onDeleteWebSearchKey={handleDeleteWebSearchKey}
            onLoadProviderModels={loadProviderModels}
            onCheckProviderBalance={checkProviderBalance}
            onPreviewTts={handlePreviewTts}
            onRefreshDiagnostics={handleRefreshDiagnostics}
            onSaveProviderCredential={saveCatalogProviderCredential}
            onDeleteProviderCredential={deleteCatalogProviderCredential}
            onVerifyCatalogProvider={verifyCatalogProvider}
            onCreateCatalogModel={createManagedModel}
            onUpdateCatalogModel={updateManagedModel}
            onDeleteCatalogModel={deleteManagedModel}
            updateProvider={updateProvider}
            updateSection={updateSection}
            updateSpeechRecognition={updateSpeechRecognition}
            providersForPurpose={providersForPurpose}
            providerAllowsCustomBase={providerAllowsCustomBase}
            providerSupportsBalance={providerSupportsBalance}
            selectedCatalogModel={selectedCatalogModel}
            capabilityBadges={capabilityBadges}
            />
          </Suspense>
        </LazySurfaceBoundary>
      )}

      {modelPicker && !modelPicker.commitOnSelect && (
        <section
          className="modal-shell stacked"
          ref={modelPickerModalRef}
          tabIndex={-1}
          role="dialog"
          aria-modal="true"
          aria-label="选择模型"
        >
          <div className="model-picker-modal">
            <header>
              <span>
                <strong>选择模型</strong>
                <small>{modelFetchStatus[modelPicker.purpose] || '来自提供商模型列表接口。'}</small>
              </span>
              <button type="button" onClick={() => setModelPicker(null)} disabled={busy}>
                关闭
              </button>
            </header>
            <div className="model-picker-list">
              {modelPicker.models.map((model) => {
                const providerName =
                  modelCatalog?.providers.find((provider) => provider.id === model.provider_id)
                    ?.name ?? model.provider_id;
                const current =
                  model.provider_id === modelPicker.currentProvider &&
                  model.model === modelPicker.currentModel;
                return (
                  <button
                    type="button"
                    key={model.id}
                    title={`${providerName} / ${model.name} · ${model.model}`}
                    aria-pressed={current}
                    disabled={busy}
                    onClick={() => void chooseFetchedModel(modelPicker.purpose, model)}
                  >
                    <span className="model-picker-identity">
                      <em>{providerName}</em>
                      <strong>{model.name}</strong>
                      <small>{model.model}</small>
                    </span>
                    <span className="model-picker-meta">
                      {current && <em className="model-picker-current">当前</em>}
                      {capabilityBadges(model)}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        </section>
      )}
      </section>
      </main>
    </>
  );
}
