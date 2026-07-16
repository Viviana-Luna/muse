import type {
  ActivePersonaResponse,
  Message,
  PersonaListResponse,
  RuntimeContextSnapshot,
  RuntimeSessionListResponse,
  RuntimeTodoItem,
  RuntimeTokenUsageResponse
} from '@/types';

export type ResourceState<T> =
  | { status: 'initial' }
  | { status: 'loading' }
  | { status: 'ready'; data: T }
  | { status: 'refreshing'; data: T }
  | { status: 'failed'; error: string }
  | { status: 'stale'; data: T; error: string };

export type OperationState =
  | { status: 'idle' }
  | { status: 'pending' }
  | { status: 'succeeded' }
  | { status: 'failed'; error: string };

export interface StoryPersonaSnapshot {
  stateRevision: number;
  personas: PersonaListResponse;
  activePersonaId: string | null;
  active: ActivePersonaResponse | null;
}

/**
 * 首页角色、会话与历史的单次提交快照。
 * 所有字段必须来自同一个稳定 revision，任何子读取失败都不能提前写入页面状态。
 */
export interface StoryRuntimeSnapshot extends StoryPersonaSnapshot {
  sessions: RuntimeSessionListResponse;
  activeConversationId: string;
  selectedConversationId: string;
  history: Message[];
  todos: RuntimeTodoItem[];
  tokenUsage: RuntimeTokenUsageResponse;
  contextSnapshot: RuntimeContextSnapshot | null;
}

export interface RevisionBoundActivePersona {
  stateRevision: number;
  active: ActivePersonaResponse | null;
}

export type PersonaPresenceState =
  | { kind: 'empty_library' }
  | { kind: 'no_active_persona'; personaCount: number }
  | { kind: 'active_persona'; active: ActivePersonaResponse };

export function resourceHasData<T>(
  state: ResourceState<T>
): state is Extract<ResourceState<T>, { data: T }> {
  return state.status === 'ready' || state.status === 'refreshing' || state.status === 'stale';
}

export function derivePersonaPresence(
  state: ResourceState<StoryPersonaSnapshot>
): PersonaPresenceState | null {
  if (!resourceHasData(state)) return null;
  if (state.data.personas.personas.length === 0) return { kind: 'empty_library' };
  if (!state.data.active) {
    return {
      kind: 'no_active_persona',
      personaCount: state.data.personas.personas.length
    };
  }
  return { kind: 'active_persona', active: state.data.active };
}
