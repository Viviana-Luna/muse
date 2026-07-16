// 运行时相关类型，描述流式事件、工具调用事件和后续运行底座协议对象。

// 运行时事件状态。
export type RuntimeEventState =
  | 'active'
  | 'completed'
  | 'error'
  | 'waiting_approval'
  | 'waiting_user'
  | string;

// 运行时事件阶段。
export type RuntimeEventPhase =
  | 'turn_started'
  | 'reasoning'
  | 'queued'
  | 'thinking'
  | 'generating'
  | 'tool_running'
  | 'tool_completed'
  | 'approval_pending'
  | 'approval_resolved'
  | 'user_question_pending'
  | 'user_question_resolved'
  | 'speech_started'
  | 'speech_finished'
  | 'synthesizing'
  | 'completed'
  | 'failed'
  | string;

// 工具风险等级。
export type ToolRisk =
  | 'read_only'
  | 'model_asset'
  | 'network'
  | 'write_file'
  | 'execute_command'
  | 'external_side_effect'
  | string;

// 工具执行归属，用于诊断模型可见定义和真实执行位置。
export type ToolExecutionOwner =
  | 'core'
  | 'web_runtime'
  | 'external_provider'
  | 'disabled'
  | string;

// 运行时事件公共字段。
export interface RuntimeEventBase {
  phase?: RuntimeEventPhase;
  message?: string;
  detail?: string | null;
  state?: RuntimeEventState;
}

// 单轮运行开始事件。
export interface RuntimeTurnStartedEvent extends RuntimeEventBase {
  type: 'turn_started';
  turn_id?: string;
  conversation_id?: string;
  persona_id?: string | null;
  model?: string;
  voice_enabled?: boolean;
  active_voice_id?: string | null;
  runtime_mode?: RuntimeMode;
  focus_phase?: RuntimeFocusPhase;
  tool_preset?: RuntimeToolPreset;
}

// 助手文本增量事件。
export interface RuntimeAssistantDeltaEvent extends RuntimeEventBase {
  type: 'assistant_delta';
  content?: string;
}

// 模型推理内容增量事件。
export interface RuntimeReasoningEvent extends RuntimeEventBase {
  type: 'reasoning_delta';
  content?: string;
}

// 助手新片段开始事件。
export interface RuntimeAssistantSegmentStartedEvent extends RuntimeEventBase {
  type: 'assistant_segment_started';
}

// 助手完整消息事件。
export interface RuntimeAssistantMessageEvent extends RuntimeEventBase {
  type: 'assistant_message';
  content?: string;
}

// 情绪状态事件。
export interface RuntimeEmotionEvent extends RuntimeEventBase {
  type: 'emotion';
  emotion?: string;
}

// 工具调用事件。
export interface RuntimeToolCallEvent extends RuntimeEventBase {
  type: 'tool_call';
  call_id?: string;
  name?: string;
  arguments?: unknown;
  risk?: ToolRisk;
  requires_approval?: boolean;
  execution_owner?: ToolExecutionOwner;
  available?: boolean;
  disabled_reason?: string | null;
  interrupt_behavior?: 'block' | 'cancel' | string;
}

// 工具执行结果事件。
export interface RuntimeToolResultEvent extends RuntimeEventBase {
  type: 'tool_result';
  call_id?: string;
  name?: string;
  success?: boolean;
  content?: string;
  structured?: unknown;
}

// 工具输出增量事件。
export interface RuntimeToolOutputDeltaEvent extends RuntimeEventBase {
  type: 'tool_output_delta';
  call_id?: string;
  name?: string;
  stream?: 'stdout' | 'stderr' | string;
  content?: string;
}

// Token 用量来源。
export type TokenUsageSource = 'provider_reported' | 'local_estimated' | string;

// 单次模型调用 Token 用量。
export interface RuntimeTokenUsage {
  id: string;
  conversation_id: string;
  turn_id: string;
  provider: string;
  model: string;
  created_at: string;
  input_tokens: number;
  output_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
  reasoning_tokens: number;
  server_tool_tokens: number;
  total_tokens: number;
  source: TokenUsageSource;
  raw_usage?: unknown;
}

// 上下文窗口来源片段。
export interface RuntimeContextSegment {
  kind: string;
  label: string;
  tokens: number;
  source: TokenUsageSource;
  compacted?: boolean;
  externalized?: boolean;
  metadata?: unknown;
}

// 当前会话上下文窗口快照。
export interface RuntimeContextSnapshot {
  conversation_id: string;
  turn_id: string;
  provider: string;
  model: string;
  created_at: string;
  context_window: number;
  reserved_output_tokens: number;
  used_input_tokens: number;
  used_cache_tokens: number;
  used_total_tokens: number;
  remaining_tokens: number;
  usage_percent: number;
  source: TokenUsageSource;
  compacted: boolean;
  externalized_tool_results: boolean;
  segments: RuntimeContextSegment[];
}

// Token 用量流式事件。
export interface RuntimeTokenUsageEvent extends RuntimeEventBase {
  type: 'token_usage';
  usage?: RuntimeTokenUsage;
}

// 上下文快照流式事件。
export interface RuntimeContextSnapshotEvent extends RuntimeEventBase {
  type: 'context_snapshot';
  snapshot?: RuntimeContextSnapshot;
}

// Token 用量聚合字段。
export interface RuntimeTokenUsageBreakdown {
  input_tokens: number;
  output_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
  reasoning_tokens: number;
  server_tool_tokens: number;
  total_tokens: number;
}

// 按来源聚合的 Token 用量。
export interface RuntimeTokenUsageSourceSummary {
  source: string;
  records: number;
  total_tokens: number;
}

// 按模型聚合的 Token 用量。
export interface RuntimeTokenUsageModelSummary {
  provider: string;
  model: string;
  records: number;
  total_tokens: number;
}

// Token 用量接口响应。
export interface RuntimeTokenUsageResponse {
  conversation_id?: string;
  range: string;
  from?: string | null;
  to: string;
  records: number;
  totals: RuntimeTokenUsageBreakdown;
  by_source: RuntimeTokenUsageSourceSummary[];
  by_model: RuntimeTokenUsageModelSummary[];
  items: RuntimeTokenUsage[];
  status: string;
}

// 上下文快照接口响应。
export interface RuntimeContextSnapshotResponse {
  conversation_id: string;
  snapshot?: RuntimeContextSnapshot | null;
  status: string;
}

// 通用状态事件。
export interface RuntimeStatusEvent extends RuntimeEventBase {
  type: 'status';
}

// 工具审批等待事件。
export interface RuntimeApprovalEvent extends RuntimeEventBase {
  type: 'approval_pending';
  approval_id?: string;
  call_id?: string;
  name?: string;
  risk?: ToolRisk;
  arguments?: unknown;
  reason?: string;
}

// 工具审批完成事件。
export interface RuntimeApprovalResolvedEvent extends RuntimeEventBase {
  type: 'approval_resolved';
  approval_id?: string;
  approved?: boolean;
  reason?: string;
}

// 用户问题选项。
export interface RuntimeUserQuestionOption {
  label: string;
  description: string;
}

// 用户问题条目。
export interface RuntimeUserQuestionItem {
  question: string;
  header: string;
  options: RuntimeUserQuestionOption[];
  multiSelect?: boolean;
  multi_select?: boolean;
}

// 等待用户回答的问题事件。
export interface RuntimeUserQuestionEvent extends RuntimeEventBase {
  type: 'user_question_pending';
  request_id?: string;
  call_id?: string;
  name?: string;
  message?: string;
  questions?: RuntimeUserQuestionItem[];
  arguments?: unknown;
}

// 用户问题完成事件。
export interface RuntimeUserQuestionResolvedEvent extends RuntimeEventBase {
  type: 'user_question_resolved';
  request_id?: string;
  answered?: boolean;
  reason?: string;
}

// 语音播报状态事件。
export interface RuntimeSpeechEvent extends RuntimeEventBase {
  type: 'speech_started' | 'speech_finished';
  call_id?: string;
  text?: string;
  voice_id?: string | null;
  success?: boolean;
}

// 单轮运行完成事件。
export interface RuntimeDoneEvent extends RuntimeEventBase {
  type: 'done';
}

// 运行错误事件。
export interface RuntimeErrorEvent extends RuntimeEventBase {
  type: 'error';
  content?: string;
}

// 运行底座工作区根目录。
export interface RuntimeWorkspaceRoot {
  path: string;
  label: string;
  kind: 'project' | 'custom' | string;
  exists: boolean;
  writable: boolean;
  removable: boolean;
}

// 运行底座工作区策略响应。
export interface RuntimeWorkspacesResponse {
  roots: RuntimeWorkspaceRoot[];
  permission_mode: 'request_approval' | 'approve_for_me' | 'full_access' | string;
  sandbox_mode: 'workspace_write' | 'danger_full_access' | string;
}

// 运行模式。
export type RuntimeMode = 'daily' | 'focus' | string;

// 专注模式阶段。
export type RuntimeFocusPhase = 'plan' | 'build' | string;

// 工具预设。
export type RuntimeToolPreset = 'daily' | 'focus_plan' | 'focus_build' | string;

// 当前运行模式响应。
export interface RuntimeModeResponse {
  mode: RuntimeMode;
  focus_phase: RuntimeFocusPhase;
  tool_preset: RuntimeToolPreset;
  status: string;
}

// 当前运行时任务清单条目。
export interface RuntimeTodoItem {
  id: string;
  content: string;
  status: 'pending' | 'in_progress' | 'completed' | string;
  priority?: 'high' | 'medium' | 'low' | string;
}

// 当前运行时任务清单响应。
export interface RuntimeTodosResponse {
  todos: RuntimeTodoItem[];
  status: string;
}

// 通用运行状态响应。
export interface RuntimeStatusResponse {
  status: string;
}

// 运行时唯一事实快照。所有跨角色、会话的异步结果都必须经过 state_revision 门闩。
export interface RuntimeStateResponse {
  state_revision: number;
  active_persona_id: string | null;
  active_conversation_id: string;
  mode: RuntimeMode;
  focus_phase: RuntimeFocusPhase;
  busy_turn: {
    turn_id: string;
    phase: string;
  } | null;
  exclusive_operation: string | null;
  usage_summary: Record<string, unknown>;
  context_summary: Record<string, unknown>;
}

// 运行时会话列表项。
export interface RuntimeSessionItem {
  conversation_id: string;
  persona_id?: string;
  persona_name_snapshot?: string;
  persona_version_snapshot?: string;
  persona_status?: 'bound' | 'missing';
  summary?: string;
  first_prompt?: string | null;
  source_conversation_id?: string | null;
  path?: string;
  exists: boolean;
  can_resume: boolean;
  records: number;
  created_time?: string | null;
  last_time?: string | null;
  archived?: boolean;
  metadata_updated_at?: string | null;
}

export interface RuntimeSessionMetadataResponse {
  conversation_id: string;
  persona_id: string;
  persona_name_snapshot: string;
  persona_version_snapshot: string;
  persona_status: 'bound' | 'missing';
  title: string | null;
  archived: boolean;
  source_conversation_id: string | null;
  updated_at: string;
  revision: number;
}

export interface RuntimeSessionExportResponse {
  schema_version: string;
  conversation_id: string;
  persona_id: string;
  persona_name_snapshot: string;
  persona_version_snapshot: string;
  persona_status: 'bound' | 'missing';
  title: string | null;
  archived: boolean;
  source_conversation_id: string | null;
  exported_at: string;
  messages: Array<{ role: 'user' | 'assistant'; content: string }>;
}

export interface RuntimeSessionContextResponse {
  conversation_id: string;
  context_snapshot: RuntimeContextSnapshot | null;
  runtime_policy_snapshot: Record<string, unknown> | null;
  status: string;
}

// 运行时会话列表响应。
export interface RuntimeSessionListResponse {
  sessions: RuntimeSessionItem[];
  active_conversation_id: string;
  status: string;
}

// 运行时会话恢复响应。
export interface RuntimeSessionResumeResponse {
  conversation_id: string;
  persona_id: string;
  persona_name_snapshot: string;
  persona_version_snapshot: string;
  persona_status: 'bound';
  restored_messages: number;
  status: string;
}

// 运行时会话分叉响应。
export interface RuntimeSessionForkResponse {
  conversation_id: string;
  persona_id: string;
  persona_name_snapshot: string;
  persona_version_snapshot: string;
  persona_status: 'bound';
  source_conversation_id: string;
  before_user_message_index?: number | null;
  restored_messages: number;
  status: string;
}

// 运行时会话删除响应。
export interface RuntimeSessionDeleteResponse {
  conversation_id: string;
  active_conversation_id: string;
  deleted_records: number;
  deleted_files: number;
  status: string;
}

// 前端可接收的运行时事件联合类型。
export type RuntimeEvent =
  | RuntimeTurnStartedEvent
  | RuntimeAssistantDeltaEvent
  | RuntimeReasoningEvent
  | RuntimeAssistantSegmentStartedEvent
  | RuntimeAssistantMessageEvent
  | RuntimeEmotionEvent
  | RuntimeToolCallEvent
  | RuntimeToolResultEvent
  | RuntimeToolOutputDeltaEvent
  | RuntimeTokenUsageEvent
  | RuntimeContextSnapshotEvent
  | RuntimeStatusEvent
  | RuntimeApprovalEvent
  | RuntimeApprovalResolvedEvent
  | RuntimeUserQuestionEvent
  | RuntimeUserQuestionResolvedEvent
  | RuntimeSpeechEvent
  | RuntimeDoneEvent
  | RuntimeErrorEvent;
