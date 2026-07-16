import type { RuntimeEvent } from '@/types';

import { apiErrorFromResponse, apiFetch } from './client';

export interface RuntimeChatStreamRequest {
  message: string;
  conversation_id: string;
  client_request_id: string;
  voice_enabled?: boolean;
}

export interface RuntimeChatStreamOptions {
  signal: AbortSignal;
  onEvent: (event: RuntimeEvent) => void;
}

function eventData(block: string): string | null {
  const lines = block.split(/\r?\n/u);
  const data = lines
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).replace(/^ /u, ''));
  return data.length > 0 ? data.join('\n') : null;
}

export function consumeSseBuffer(
  buffer: string,
  onEvent: (event: RuntimeEvent) => void,
  flush = false
): string {
  let rest = buffer;
  while (rest) {
    const separator = rest.match(/\r?\n\r?\n/u);
    if (!separator || separator.index == null) break;
    const block = rest.slice(0, separator.index);
    rest = rest.slice(separator.index + separator[0].length);
    emitSseBlock(block, onEvent);
  }
  if (flush && rest.trim()) {
    emitSseBlock(rest, onEvent);
    return '';
  }
  return rest;
}

function emitSseBlock(block: string, onEvent: (event: RuntimeEvent) => void) {
  const data = eventData(block);
  if (!data) return;
  if (data === '[DONE]') {
    onEvent({ type: 'done' });
    return;
  }
  try {
    onEvent(JSON.parse(data) as RuntimeEvent);
  } catch {
    throw new Error('运行时返回了无法解析的流式事件。');
  }
}

export async function streamRuntimeChat(
  request: RuntimeChatStreamRequest,
  { signal, onEvent }: RuntimeChatStreamOptions
): Promise<void> {
  const response = await apiFetch('/api/chat/stream', {
    method: 'POST',
    headers: {
      Accept: 'text/event-stream',
      'Content-Type': 'application/json'
    },
    body: JSON.stringify(request),
    signal
  });
  if (!response.ok) {
    throw await apiErrorFromResponse(response, `无法建立流式连接（HTTP ${response.status}）。`);
  }
  if (!response.body) {
    throw new Error('当前浏览器无法读取流式响应。');
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      buffer = consumeSseBuffer(buffer, onEvent);
    }
    buffer += decoder.decode();
    consumeSseBuffer(buffer, onEvent, true);
  } finally {
    reader.releaseLock();
  }
}
