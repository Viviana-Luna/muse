import { useMemo, useState } from 'react';
import type { Dispatch, SetStateAction } from 'react';

import type {
  DiagnosticsConnectivityItem,
  ModelCatalog,
  ModelCatalogItem,
  ModelConfigSection,
  ModelsConfig
} from '@/types';
import type { SecretUpdate, WebSearchProvider } from '@/types';
import {
  DEFAULT_APPEARANCE_SETTINGS,
  type AppearanceSettings,
  type ModelPurpose,
  type SettingsDirtyDomain,
  type SettingsPanel
} from '@/views/settings/types';

const APPEARANCE_STORAGE_KEY = 'muse:appearance-settings';
const LEGACY_APPEARANCE_STORAGE_KEY = 'agent-vp:appearance-settings';

export interface SettingsMaskedKeys {
  chat: string;
  tts: string;
  asr: string;
  audio_understanding: string;
}

export interface SettingsModelPicker {
  purpose: ModelPurpose;
  models: ModelCatalogItem[];
  currentProvider?: string;
  currentModel?: string;
  commitOnSelect?: boolean;
}

export interface LoadedSettingsDraft {
  config: ModelsConfig;
  catalog: ModelCatalog;
  permissionMode: string;
  sandboxMode: string;
  webSearchProvider: WebSearchProvider;
  webSearchConfigured: boolean;
  panel: SettingsPanel;
}

export interface UseSettingsDraftResult {
  settingsOpen: boolean;
  setSettingsOpen: Dispatch<SetStateAction<boolean>>;
  settingsPanel: SettingsPanel;
  setSettingsPanel: Dispatch<SetStateAction<SettingsPanel>>;
  settingsConfig: ModelsConfig | null;
  setSettingsConfig: Dispatch<SetStateAction<ModelsConfig | null>>;
  workspacePermissionMode: string;
  setWorkspacePermissionMode: Dispatch<SetStateAction<string>>;
  workspaceSandboxMode: string;
  setWorkspaceSandboxMode: Dispatch<SetStateAction<string>>;
  webSearchConfigured: boolean;
  setWebSearchConfigured: Dispatch<SetStateAction<boolean>>;
  webSearchProvider: WebSearchProvider;
  stageWebSearchProvider: (provider: WebSearchProvider) => void;
  webSearchKeyDraft: string;
  stageWebSearchKey: (value: string) => void;
  stageWebSearchDelete: () => void;
  webSearchAction: SecretUpdate['action'];
  appearanceSettings: AppearanceSettings;
  legacyAppearanceSettings: AppearanceSettings | null;
  setAppearanceSettings: Dispatch<SetStateAction<AppearanceSettings>>;
  modelCatalog: ModelCatalog | null;
  setModelCatalog: Dispatch<SetStateAction<ModelCatalog | null>>;
  diagnosticsChecks: DiagnosticsConnectivityItem[];
  setDiagnosticsChecks: Dispatch<SetStateAction<DiagnosticsConnectivityItem[]>>;
  modelPicker: SettingsModelPicker | null;
  setModelPicker: Dispatch<SetStateAction<SettingsModelPicker | null>>;
  modelFetchStatus: Record<ModelPurpose, string>;
  setModelFetchStatus: Dispatch<SetStateAction<Record<ModelPurpose, string>>>;
  maskedKeys: SettingsMaskedKeys;
  setMaskedKeys: Dispatch<SetStateAction<SettingsMaskedKeys>>;
  applyLoadedSettings: (draft: LoadedSettingsDraft) => void;
  applyWebSearchState: (provider: WebSearchProvider, configured: boolean) => void;
  dirtyDomains: SettingsDirtyDomain[];
  markModelsSaved: (config: ModelsConfig) => void;
  markWorkspaceSaved: (permissionMode: string, sandboxMode: string) => void;
  markAppearanceSaved: (settings?: AppearanceSettings) => void;
  stageWorkspacePolicy: (permissionMode: string, sandboxMode: string) => void;
  discardSettingsChanges: () => void;
  updateSection: <K extends keyof ModelConfigSection>(
    section: ModelPurpose,
    key: K,
    value: ModelConfigSection[K]
  ) => void;
  updateSpeechRecognition: <K extends keyof ModelsConfig['speech_recognition']>(
    key: K,
    value: ModelsConfig['speech_recognition'][K]
  ) => void;
}

interface InitialAppearanceState {
  settings: AppearanceSettings;
  legacy: AppearanceSettings | null;
}

function readAppearanceSettings(): InitialAppearanceState {
  if (typeof window === 'undefined') {
    return { settings: DEFAULT_APPEARANCE_SETTINGS, legacy: null };
  }
  const stored =
    window.localStorage.getItem(APPEARANCE_STORAGE_KEY) ??
    window.localStorage.getItem(LEGACY_APPEARANCE_STORAGE_KEY);
  if (!stored) return { settings: DEFAULT_APPEARANCE_SETTINGS, legacy: null };
  try {
    const parsed = JSON.parse(stored) as Partial<AppearanceSettings>;
    const settings = {
      backgroundBlur:
        typeof parsed.backgroundBlur === 'number'
          ? Math.min(30, Math.max(0, parsed.backgroundBlur))
          : DEFAULT_APPEARANCE_SETTINGS.backgroundBlur,
      backgroundOpacity:
        typeof parsed.backgroundOpacity === 'number'
          ? Math.min(1, Math.max(0.2, parsed.backgroundOpacity))
          : DEFAULT_APPEARANCE_SETTINGS.backgroundOpacity,
      motionLevel:
        parsed.motionLevel === 'reduced' || parsed.motionLevel === 'none'
          ? parsed.motionLevel
          : DEFAULT_APPEARANCE_SETTINGS.motionLevel
    };
    return { settings, legacy: settings };
  } catch {
    return { settings: DEFAULT_APPEARANCE_SETTINGS, legacy: null };
  }
}

export function useSettingsDraft(): UseSettingsDraftResult {
  const [initialAppearance] = useState(readAppearanceSettings);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsPanel, setSettingsPanel] = useState<SettingsPanel>('chat');
  const [settingsConfig, setSettingsConfig] = useState<ModelsConfig | null>(null);
  const [workspacePermissionMode, setWorkspacePermissionMode] = useState('request_approval');
  const [workspaceSandboxMode, setWorkspaceSandboxMode] = useState('workspace_write');
  const [webSearchConfigured, setWebSearchConfigured] = useState(false);
  const [webSearchProvider, setWebSearchProvider] =
    useState<WebSearchProvider>('exa_free_mcp');
  const [savedWebSearchProvider, setSavedWebSearchProvider] =
    useState<WebSearchProvider>('exa_free_mcp');
  const [webSearchKeyDraft, setWebSearchKeyDraft] = useState('');
  const [webSearchAction, setWebSearchAction] = useState<SecretUpdate['action']>('keep');
  const [appearanceSettings, setAppearanceSettings] =
    useState<AppearanceSettings>(initialAppearance.settings);
  const [legacyAppearanceSettings, setLegacyAppearanceSettings] =
    useState<AppearanceSettings | null>(initialAppearance.legacy);
  const [modelCatalog, setModelCatalog] = useState<ModelCatalog | null>(null);
  const [diagnosticsChecks, setDiagnosticsChecks] = useState<DiagnosticsConnectivityItem[]>([]);
  const [modelPicker, setModelPicker] = useState<SettingsModelPicker | null>(null);
  const [modelFetchStatus, setModelFetchStatus] = useState<Record<ModelPurpose, string>>({
    chat: ''
  });
  const [maskedKeys, setMaskedKeys] = useState<SettingsMaskedKeys>({
    chat: '',
    tts: '',
    asr: '',
    audio_understanding: ''
  });
  const [savedModelsSnapshot, setSavedModelsSnapshot] = useState('');
  const [savedWorkspaceSnapshot, setSavedWorkspaceSnapshot] = useState('');
  const [savedAppearanceSnapshot, setSavedAppearanceSnapshot] = useState(() =>
    JSON.stringify(initialAppearance.settings)
  );

  const dirtyDomains = useMemo<SettingsDirtyDomain[]>(() => {
    const dirty: SettingsDirtyDomain[] = [];
    const modelsSnapshot = settingsConfig ? JSON.stringify(settingsConfig) : '';
    if (settingsConfig && modelsSnapshot !== savedModelsSnapshot) dirty.push('models');
    if (
      JSON.stringify([workspacePermissionMode, workspaceSandboxMode]) !== savedWorkspaceSnapshot
    ) {
      dirty.push('workspace');
    }
    if (webSearchAction !== 'keep' || webSearchProvider !== savedWebSearchProvider) {
      dirty.push('web_search');
    }
    if (JSON.stringify(appearanceSettings) !== savedAppearanceSnapshot) dirty.push('appearance');
    return dirty;
  }, [
    appearanceSettings,
    savedAppearanceSnapshot,
    savedModelsSnapshot,
    savedWorkspaceSnapshot,
    settingsConfig,
    webSearchProvider,
    savedWebSearchProvider,
    webSearchAction,
    workspacePermissionMode,
    workspaceSandboxMode
  ]);

  function applyLoadedSettings(draft: LoadedSettingsDraft) {
    setMaskedKeys({
      chat: draft.config.chat.api_key_configured ? '••••••••' : '',
      tts: draft.config.tts.api_key_configured ? '••••••••' : '',
      asr: draft.config.speech_recognition.api_key_configured ? '••••••••' : '',
      audio_understanding: draft.config.audio_understanding.api_key_configured
        ? '••••••••'
        : ''
    });
    setSettingsConfig(draft.config);
    setSavedModelsSnapshot(JSON.stringify(draft.config));
    setModelCatalog(draft.catalog);
    setWorkspacePermissionMode(draft.permissionMode);
    setWorkspaceSandboxMode(draft.sandboxMode);
    setSavedWorkspaceSnapshot(JSON.stringify([draft.permissionMode, draft.sandboxMode]));
    setWebSearchProvider(draft.webSearchProvider);
    setSavedWebSearchProvider(draft.webSearchProvider);
    setWebSearchConfigured(draft.webSearchConfigured);
    setWebSearchKeyDraft('');
    setWebSearchAction('keep');
    setSettingsPanel(draft.panel);
    setSettingsOpen(true);
  }

  function applyWebSearchState(provider: WebSearchProvider, configured: boolean) {
    setWebSearchProvider(provider);
    setSavedWebSearchProvider(provider);
    setWebSearchConfigured(configured);
    setWebSearchKeyDraft('');
    setWebSearchAction('keep');
  }

  function stageWebSearchProvider(provider: WebSearchProvider) {
    setWebSearchProvider(provider);
    if (provider === 'exa_api' && webSearchAction === 'delete') {
      setWebSearchAction('keep');
    }
  }

  function stageWebSearchKey(value: string) {
    setWebSearchKeyDraft(value);
    setWebSearchAction(value.trim() ? 'replace' : 'keep');
  }

  function stageWebSearchDelete() {
    setWebSearchKeyDraft('');
    setWebSearchAction('delete');
    setWebSearchProvider('exa_free_mcp');
  }

  function stageWorkspacePolicy(permissionMode: string, sandboxMode: string) {
    setWorkspacePermissionMode(permissionMode);
    setWorkspaceSandboxMode(sandboxMode);
  }

  function markModelsSaved(config: ModelsConfig) {
    setSettingsConfig(config);
    setSavedModelsSnapshot(JSON.stringify(config));
  }

  function markWorkspaceSaved(permissionMode: string, sandboxMode: string) {
    setWorkspacePermissionMode(permissionMode);
    setWorkspaceSandboxMode(sandboxMode);
    setSavedWorkspaceSnapshot(JSON.stringify([permissionMode, sandboxMode]));
  }

  function markAppearanceSaved(settings: AppearanceSettings = appearanceSettings) {
    const snapshot = JSON.stringify(settings);
    setAppearanceSettings(settings);
    if (typeof window !== 'undefined') {
      window.localStorage.removeItem(APPEARANCE_STORAGE_KEY);
      window.localStorage.removeItem(LEGACY_APPEARANCE_STORAGE_KEY);
    }
    setLegacyAppearanceSettings(null);
    setSavedAppearanceSnapshot(snapshot);
  }

  function discardSettingsChanges() {
    if (savedModelsSnapshot) {
      setSettingsConfig(JSON.parse(savedModelsSnapshot) as ModelsConfig);
    }
    if (savedWorkspaceSnapshot) {
      const [permissionMode, sandboxMode] = JSON.parse(savedWorkspaceSnapshot) as [string, string];
      setWorkspacePermissionMode(permissionMode);
      setWorkspaceSandboxMode(sandboxMode);
    }
    setWebSearchKeyDraft('');
    setWebSearchAction('keep');
    setWebSearchProvider(savedWebSearchProvider);
    setAppearanceSettings(JSON.parse(savedAppearanceSnapshot) as AppearanceSettings);
  }

  function updateSection<K extends keyof ModelConfigSection>(
    section: ModelPurpose,
    key: K,
    value: ModelConfigSection[K]
  ) {
    setSettingsConfig((state) => {
      if (!state) return state;
      return {
        ...state,
        [section]: { ...state[section], [key]: value }
      };
    });
    if (key === 'provider' || key === 'api_base' || key === 'api_key' || key === 'api_protocol') {
      setModelFetchStatus((state) => ({
        ...state,
        [section]: '连接配置已更改，请重新验证。'
      }));
    }
  }

  function updateSpeechRecognition<K extends keyof ModelsConfig['speech_recognition']>(
    key: K,
    value: ModelsConfig['speech_recognition'][K]
  ) {
    setSettingsConfig((state) => {
      if (!state) return state;
      return {
        ...state,
        speech_recognition: {
          ...state.speech_recognition,
          [key]: value
        }
      };
    });
  }

  return {
    settingsOpen,
    setSettingsOpen,
    settingsPanel,
    setSettingsPanel,
    settingsConfig,
    setSettingsConfig,
    workspacePermissionMode,
    setWorkspacePermissionMode,
    workspaceSandboxMode,
    setWorkspaceSandboxMode,
    webSearchConfigured,
    setWebSearchConfigured,
    webSearchProvider,
    stageWebSearchProvider,
    webSearchKeyDraft,
    stageWebSearchKey,
    stageWebSearchDelete,
    webSearchAction,
    appearanceSettings,
    legacyAppearanceSettings,
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
  };
}
