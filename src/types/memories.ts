// Persona 长期记忆管理类型；正文只存在于管理页和当前模型回合，不进入 Session 导出。

export type MemoryCategory =
  | 'user_fact'
  | 'user_preference'
  | 'shared_experience'
  | 'commitment'
  | 'story_state';

export type MemoryImportance = 'low' | 'normal' | 'high';
export type MemoryFacet =
  | 'identity'
  | 'timezone'
  | 'location'
  | 'occupation'
  | 'preference_food'
  | 'preference_drink'
  | 'preference_communication'
  | 'preference_tool'
  | 'preference_other'
  | 'habit'
  | 'plan'
  | 'shared_experience'
  | 'commitment'
  | 'story_state'
  | 'other';
export type MemoryChangeType = 'create' | 'update' | 'correct';
export type MemoryRevisionState = 'current' | 'superseded' | 'corrected';

export interface MemoryQueryItem {
  memory_id: string;
  revision_id: string;
  category: MemoryCategory;
  facet: MemoryFacet;
  keywords: string[];
  content: string;
  importance: MemoryImportance;
  event_time: string | null;
  recorded_at: string;
  valid_from: string;
  valid_to: string | null;
  change_type: MemoryChangeType;
  change_reason: string;
}

export interface MemoryQueryPageReceipt {
  items: MemoryQueryItem[];
  has_more: boolean;
  next_cursor?: string;
}

export interface MemoryEntry {
  memory_id: string;
  persona_id: string;
  category: MemoryCategory;
  current_revision_id: string;
  importance: MemoryImportance;
  freshness_at: string;
  created_at: string;
  state: 'active' | 'deleted';
}

export interface MemoryRevision {
  revision_id: string;
  memory_id: string;
  facet: MemoryFacet;
  keywords: string[];
  content: string;
  event_time: string | null;
  recorded_at: string;
  valid_from: string;
  valid_to: string | null;
  change_type: MemoryChangeType;
  change_reason: string;
  source_conversation_id: string | null;
  source_turn_id: string | null;
  safety_policy_version: string;
  state: MemoryRevisionState;
}

export interface MemoryDetailResponse {
  entry: MemoryEntry;
  current_revision: MemoryRevision;
  source_conversation_id: string | null;
  source_turn_id: string | null;
}

export interface MemoryHistoryResponse {
  memory_id: string;
  revisions: MemoryRevision[];
}

export interface MemoryCreateInput {
  category: MemoryCategory;
  content: string;
  importance: MemoryImportance;
  event_time: string | null;
  change_reason: string;
  operation_id: string;
}

export interface MemoryCorrectInput {
  expected_revision_id: string;
  category: MemoryCategory;
  content: string;
  event_time: string | null;
  change_reason: string;
  operation_id: string;
}

export interface MemoryImportanceInput {
  expected_revision_id: string;
  expected_importance: MemoryImportance;
  importance: MemoryImportance;
  operation_id: string;
}

export interface MemoryMutationReceipt {
  operation: MemoryChangeType;
  memory_id: string;
  revision_id: string;
  state: 'staged' | 'durable';
}

export interface MemoryImportanceReceipt {
  operation_id: string;
  memory_id: string;
  previous_importance: MemoryImportance;
  importance: MemoryImportance;
  durable_at: string;
}

export interface MemoryDeleteReceipt {
  deletion_id: string;
  deleted_memory_count: number;
  completed_at: string;
}
