// 角色与视觉包相关类型，描述角色资料、角色卡片和角色视觉资源。

export interface ToolPolicy {
  mode: 'inherit' | 'disabled' | 'allow_list';
  allowed_tools: string[];
}

export interface SkillPolicy {
  mode: 'inherit' | 'disabled' | 'allow_list';
  allowed_skills: string[];
}

export interface McpPolicy {
  mode: 'inherit' | 'disabled' | 'allow_list';
  allowed_servers: string[];
}

export type RoleplayStyle = 'dialogue' | 'light_narration' | 'immersive' | 'text_adventure';
export type VisualThemeMode = 'auto' | 'dark' | 'light';

export interface PersonaModelReference {
  provider_id: string;
  model_id: string;
}

export interface PersonaFeaturePolicy {
  emotion_persistence_enabled: boolean;
}

export interface Persona {
  id: string;
  name: string;
  summary: string;
  character_profile: string;
  world_profile: string;
  scenario: string;
  system_prompt: string;
  style: string;
  roleplay_style: RoleplayStyle;
  dialogue_examples: string;
  author_note: string;
  opening_message: string;
  tool_policy: ToolPolicy;
  skill_policy: SkillPolicy;
  mcp_policy: McpPolicy;
  preferred_model_ref: PersonaModelReference | null;
  preferred_voice_id: string | null;
  feature_policy: PersonaFeaturePolicy;
  default_visual_pack_id: string;
  author: string;
  version: string;
  notes: string;
}

export interface PersonaSummary {
  id: string;
  name: string;
  summary: string;
  default_visual_pack_id: string;
  author: string;
  version: string;
}

export interface PersonaVisualPreview {
  avatar_path: string | null;
  portrait_path: string | null;
}

export interface PersonaLibraryItem extends PersonaSummary {
  visual_preview: PersonaVisualPreview;
}

export interface VisualPack {
  id: string;
  name: string;
  portrait_path: string;
  background_path: string;
  avatar_path: string;
  theme_color: string;
  theme_mode: VisualThemeMode;
  layout_mode: string;
  portrait_frame: string;
  portrait_fit: string;
  portrait_position_x: number;
  portrait_position_y: number;
  portrait_scale: number;
  fallback_text: string;
  version: string;
  notes: string;
}

export interface PersonaVisualPackPatch {
  portrait_path: string;
  background_path?: string;
  avatar_path?: string;
  theme_color?: string;
  theme_mode?: VisualThemeMode;
  portrait_frame?: string;
  portrait_fit?: string;
  portrait_position_x?: number;
  portrait_position_y?: number;
  portrait_scale?: number;
}

export interface AssetUploadResponse {
  url: string;
}

export interface ActivePersonaResponse {
  persona: Persona;
  visual_pack: VisualPack | null;
  runtime_state?: PersonaRuntimeState | null;
}

export interface PersonaRuntimeState {
  persona_id: string;
  emotion: 'happy' | 'sad' | 'surprised' | 'thinking' | 'neutral' | 'angry';
  intensity: number;
  reason_code:
    | 'positive_interaction'
    | 'negative_interaction'
    | 'surprise'
    | 'deliberation'
    | 'conflict'
    | 'neutral';
  source_conversation_id: string;
  source_turn_id: string;
  last_interaction_at: string;
  revision: number;
  effective_emotion: string;
  effective_intensity: number;
  evaluated_at: string;
}

/**
 * 当前角色事实快照。没有活动角色是合法产品状态，因此接口始终返回 200，
 * 调用方必须结合显式资源状态判断 loading，不能再由 null 反推加载中。
 */
export interface ActivePersonaStateResponse {
  active_persona: Persona | null;
  active_persona_id: string | null;
  visual_pack: VisualPack | null;
  state_revision: number;
}

export interface PersonaListResponse {
  personas: PersonaLibraryItem[];
  active_persona_id: string | null;
}

export interface PersonaMutationResponse {
  affected_persona: Persona;
  active_persona: Persona | null;
  active_persona_id: string | null;
  visual_pack: VisualPack | null;
  runtime_reset: boolean;
  conversation_id: string;
  active_conversation_id: string;
  session_restored: boolean;
  state_revision: number;
}

export interface PersonaDeletionImpactResponse {
  persona_id: string;
  associated_session_count: number;
  workspace_state_exists: boolean;
}

export interface PersonaCard {
  schema_version: string;
  exported_at: string;
  export_level: 'persona_only' | 'with_visual_pack_ref';
  persona: Persona;
  visual_pack?: VisualPack | null;
}

export interface PersonaCardImportResponse {
  affected_persona: Persona;
  active_persona: Persona | null;
  active_persona_id: string | null;
  visual_pack: VisualPack | null;
  notices: string[];
  runtime_reset: boolean;
  conversation_id: string;
  state_revision: number;
}
