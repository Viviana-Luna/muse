import { apiFetch, readJson } from './client';
import type {
  RuntimeFocusPhase,
  RuntimeContextSnapshotResponse,
  RuntimeMode,
  RuntimeModeResponse,
  RuntimeSessionDeleteResponse,
  RuntimeSessionForkResponse,
  RuntimeSessionListResponse,
  RuntimeSessionMetadataResponse,
  RuntimeSessionExportResponse,
  RuntimeSessionContextResponse,
  RuntimeSessionResumeResponse,
  RuntimeStateResponse,
  RuntimeStatusResponse,
  RuntimeTokenUsageResponse,
  RuntimeTodosResponse,
  RuntimeWorkspacesResponse
} from '@/types';

export interface ApprovalDecisionResponse {
  approval_id: string;
  approved: boolean;
  status: string;
  reason?: string;
}

export interface UserQuestionDecisionResponse {
  request_id: string;
  answered: boolean;
  status: string;
  reason?: string;
}

export interface UserQuestionAnswerPayload {
  answers: Record<string, string | string[]>;
  annotations?: Record<string, { notes?: string }>;
}

// 运行时审批 API：页面只提交用户决策，不在组件里散写工具风险判断。
export async function approveRuntimeTool(
  turnId: string,
  approvalId: string
): Promise<ApprovalDecisionResponse> {
  return readJson<ApprovalDecisionResponse>(
    await apiFetch(`/api/runtime/approvals/${encodeURIComponent(approvalId)}/approve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ turn_id: turnId })
    })
  );
}

export async function rejectRuntimeTool(
  turnId: string,
  approvalId: string,
  reason = '用户拒绝执行该工具。'
): Promise<ApprovalDecisionResponse> {
  return readJson<ApprovalDecisionResponse>(
    await apiFetch(`/api/runtime/approvals/${encodeURIComponent(approvalId)}/reject`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ turn_id: turnId, reason })
    })
  );
}

export async function cancelRuntimeTool(
  turnId: string,
  approvalId: string
): Promise<ApprovalDecisionResponse> {
  return readJson<ApprovalDecisionResponse>(
    await apiFetch(`/api/runtime/approvals/${encodeURIComponent(approvalId)}/cancel`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ turn_id: turnId })
    })
  );
}

export async function answerRuntimeUserQuestion(
  turnId: string,
  requestId: string,
  payload: UserQuestionAnswerPayload
): Promise<UserQuestionDecisionResponse> {
  return readJson<UserQuestionDecisionResponse>(
    await apiFetch(`/api/runtime/user-questions/${encodeURIComponent(requestId)}/answer`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ...payload, turn_id: turnId })
    })
  );
}

export async function cancelRuntimeUserQuestion(
  turnId: string,
  requestId: string
): Promise<UserQuestionDecisionResponse> {
  return readJson<UserQuestionDecisionResponse>(
    await apiFetch(`/api/runtime/user-questions/${encodeURIComponent(requestId)}/cancel`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ turn_id: turnId })
    })
  );
}

export async function cancelRuntimeTurn(turnId: string): Promise<RuntimeStatusResponse> {
  return readJson<RuntimeStatusResponse>(
    await apiFetch(`/api/runtime/turns/${encodeURIComponent(turnId)}/cancel`, {
      method: 'POST'
    })
  );
}

export async function fetchRuntimeWorkspaces(): Promise<RuntimeWorkspacesResponse> {
  return readJson<RuntimeWorkspacesResponse>(await apiFetch('/api/runtime/workspaces'));
}

export async function fetchRuntimeSessions(): Promise<RuntimeSessionListResponse> {
  return readJson<RuntimeSessionListResponse>(await apiFetch('/api/runtime/sessions'));
}

export async function fetchRuntimeState(signal?: AbortSignal): Promise<RuntimeStateResponse> {
  return readJson<RuntimeStateResponse>(await apiFetch('/api/runtime/state', { signal }));
}

export async function fetchRuntimeMode(): Promise<RuntimeModeResponse> {
  return readJson<RuntimeModeResponse>(await apiFetch('/api/runtime/mode'));
}

export async function fetchRuntimeTodos(): Promise<RuntimeTodosResponse> {
  return readJson<RuntimeTodosResponse>(await apiFetch('/api/runtime/todos'));
}

export async function fetchRuntimeTokenUsage(
  conversationId = 'default',
  range: 'day' | 'week' | 'all' = 'day',
  signal?: AbortSignal
): Promise<RuntimeTokenUsageResponse> {
  const params = new URLSearchParams({
    conversation_id: conversationId,
    range
  });
  return readJson<RuntimeTokenUsageResponse>(
    await apiFetch(`/api/runtime/token-usage?${params.toString()}`, { signal })
  );
}

export async function fetchRuntimeContextSnapshot(
  conversationId = 'default',
  signal?: AbortSignal
): Promise<RuntimeContextSnapshotResponse> {
  const params = new URLSearchParams({
    conversation_id: conversationId
  });
  return readJson<RuntimeContextSnapshotResponse>(
    await apiFetch(`/api/runtime/context-snapshot?${params.toString()}`, { signal })
  );
}

export async function updateRuntimeMode(
  mode: RuntimeMode,
  focusPhase?: RuntimeFocusPhase
): Promise<RuntimeModeResponse> {
  return readJson<RuntimeModeResponse>(
    await apiFetch('/api/runtime/mode', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        mode,
        focus_phase: focusPhase
      })
    })
  );
}

export async function updateRuntimeWorkspacePolicy(
  permissionMode: string,
  sandboxMode: string
): Promise<RuntimeWorkspacesResponse> {
  return readJson<RuntimeWorkspacesResponse>(
    await apiFetch('/api/runtime/workspaces', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        permission_mode: permissionMode,
        sandbox_mode: sandboxMode
      })
    })
  );
}

export async function resumeRuntimeSession(
  conversationId = 'default'
): Promise<RuntimeSessionResumeResponse> {
  return readJson<RuntimeSessionResumeResponse>(
    await apiFetch(`/api/runtime/sessions/${encodeURIComponent(conversationId)}/resume`, {
      method: 'POST'
    })
  );
}

export async function forkRuntimeSession(
  sourceConversationId = 'default',
  beforeUserMessageIndex?: number,
  targetPersonaId?: string
): Promise<RuntimeSessionForkResponse> {
  return readJson<RuntimeSessionForkResponse>(
    await apiFetch(`/api/runtime/sessions/${encodeURIComponent(sourceConversationId)}/fork`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        ...(beforeUserMessageIndex == null
          ? {}
          : { before_user_message_index: beforeUserMessageIndex }),
        ...(targetPersonaId ? { target_persona_id: targetPersonaId } : {})
      })
    })
  );
}

export async function deleteRuntimeSession(
  conversationId: string
): Promise<RuntimeSessionDeleteResponse> {
  return readJson<RuntimeSessionDeleteResponse>(
    await apiFetch(`/api/runtime/sessions/${encodeURIComponent(conversationId)}`, {
      method: 'DELETE'
    })
  );
}

export async function updateRuntimeSessionMetadata(
  conversationId: string,
  patch: { title?: string | null; archived?: boolean }
): Promise<RuntimeSessionMetadataResponse> {
  return readJson<RuntimeSessionMetadataResponse>(
    await apiFetch(`/api/runtime/sessions/${encodeURIComponent(conversationId)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(patch)
    })
  );
}

export async function exportRuntimeSession(
  conversationId: string
): Promise<RuntimeSessionExportResponse> {
  return readJson<RuntimeSessionExportResponse>(
    await apiFetch(`/api/runtime/sessions/${encodeURIComponent(conversationId)}/export`)
  );
}

export async function fetchRuntimeSessionContext(
  conversationId: string,
  signal?: AbortSignal
): Promise<RuntimeSessionContextResponse> {
  return readJson<RuntimeSessionContextResponse>(
    await apiFetch(`/api/runtime/sessions/${encodeURIComponent(conversationId)}/context`, {
      signal
    })
  );
}
