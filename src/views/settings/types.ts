import type { Dispatch, FormEvent, ReactNode, SetStateAction } from 'react';

import type {
  ModelCatalogItem,
  ModelCatalog,
  ModelCatalogMutation,
  ModelConfigSection,
  ModelProviderCatalog,
  ModelsConfig,
  DiagnosticsConnectivityItem
} from '@/types';
import type { SecretUpdate, WebSearchProvider } from '@/types';

export type SettingsPanel =
  | 'chat'
  | 'tts'
  | 'speech_recognition'
  | 'workspace'
  | 'web_search'
  | 'appearance'
  | 'diagnostics';

export type ModelPurpose = 'chat';

export type MotionLevel = 'full' | 'reduced' | 'none';
export type SettingsDirtyDomain = 'models' | 'workspace' | 'web_search' | 'appearance';

export interface AppearanceSettings {
  backgroundBlur: number;
  backgroundOpacity: number;
  motionLevel: MotionLevel;
}

export const DEFAULT_APPEARANCE_SETTINGS: AppearanceSettings = {
  backgroundBlur: 18,
  backgroundOpacity: 1,
  motionLevel: 'full'
};

export interface SettingsPanelDefinition {
  id: SettingsPanel;
  label: string;
  description: string;
  marker: string;
}

export interface SettingsDialogProps {
  embedded?: boolean;
  settingsConfig: ModelsConfig;
  modelCatalog: ModelCatalog | null;
  settingsPanel: SettingsPanel;
  busy: boolean;
  maskedKeys: {
    chat: string;
    tts: string;
    asr: string;
    audio_understanding: string;
  };
  modelFetchStatus: Record<ModelPurpose, string>;
  workspacePermissionMode: string;
  workspaceSandboxMode: string;
  webSearchProvider: WebSearchProvider;
  webSearchConfigured: boolean;
  webSearchKeyDraft: string;
  webSearchAction: SecretUpdate['action'];
  dirtyDomains: SettingsDirtyDomain[];
  runtimeStatus: string;
  voiceStatus: string;
  diagnosticsChecks: DiagnosticsConnectivityItem[];
  appearanceSettings: AppearanceSettings;
  setSettingsPanel: (panel: SettingsPanel) => void;
  setSettingsConfig: Dispatch<SetStateAction<ModelsConfig | null>>;
  setModelCatalog: Dispatch<SetStateAction<ModelCatalog | null>>;
  setAppearanceSettings: Dispatch<SetStateAction<AppearanceSettings>>;
  setWebSearchProvider: (provider: WebSearchProvider) => void;
  setWebSearchKeyDraft: (value: string) => void;
  onClose: () => void;
  onDiscard: () => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void | Promise<void>;
  onUpdateWorkspacePolicy: (permissionMode: string, sandboxMode: string) => void | Promise<void>;
  onDeleteWebSearchKey: () => void | Promise<void>;
  onLoadProviderModels: (purpose: ModelPurpose) => void | Promise<void>;
  onCheckProviderBalance: (purpose: ModelPurpose) => void | Promise<void>;
  onPreviewTts: () => void | Promise<void>;
  onRefreshDiagnostics: () => void | Promise<void>;
  onSaveProviderCredential: (providerId: string, secret: string) => void | Promise<void>;
  onDeleteProviderCredential: (providerId: string) => void | Promise<void>;
  onSetProviderEnabled: (providerId: string, enabled: boolean) => void | Promise<void>;
  onVerifyCatalogProvider: (providerId: string, model?: string) => void | Promise<void>;
  onCreateCatalogModel: (model: ModelCatalogMutation) => void | Promise<void>;
  onUpdateCatalogModel: (model: ModelCatalogMutation) => void | Promise<void>;
  onDeleteCatalogModel: (providerId: string, model: string) => void | Promise<void>;
  updateProvider: (purpose: ModelPurpose, providerId: string) => void;
  updateSection: <K extends keyof ModelConfigSection>(
    section: ModelPurpose,
    key: K,
    value: ModelConfigSection[K]
  ) => void;
  updateSpeechRecognition: <K extends keyof ModelsConfig['speech_recognition']>(
    key: K,
    value: ModelsConfig['speech_recognition'][K]
  ) => void;
  providersForPurpose: (purpose: ModelPurpose) => ModelProviderCatalog[];
  providerAllowsCustomBase: (providerId: string) => boolean;
  providerSupportsBalance: (providerId: string) => boolean;
  selectedCatalogModel: (
    purpose: ModelPurpose,
    section: ModelConfigSection
  ) => ModelCatalogItem | null | undefined;
  capabilityBadges: (model?: ModelCatalogItem | null) => ReactNode;
}
