import type { Message } from '@/types';

import { apiFetch, readJson } from './client';

// 聊天会话 API，负责历史消息读取和当前会话重置。
export interface ResetConversationResponse {
  status: string;
  conversation_id: string;
}

export async function fetchHistory(conversationId?: string, signal?: AbortSignal): Promise<Message[]> {
  const url = conversationId ? `/api/history?conversation_id=${encodeURIComponent(conversationId)}` : '/api/history';
  const payload = await readJson<{ messages: Message[] }>(await apiFetch(url, { signal }));
  return payload.messages;
}

export async function resetConversation(): Promise<ResetConversationResponse> {
  return readJson<ResetConversationResponse>(await apiFetch('/api/reset', { method: 'POST' }));
}
