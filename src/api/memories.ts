import type {
  MemoryCategory,
  MemoryCorrectInput,
  MemoryCreateInput,
  MemoryDeleteReceipt,
  MemoryDetailResponse,
  MemoryHistoryResponse,
  MemoryImportance,
  MemoryImportanceInput,
  MemoryImportanceReceipt,
  MemoryMutationReceipt,
  MemoryQueryPageReceipt
} from '@/types';

import { apiFetch, readJson } from './client';

function personaMemoryPath(personaId: string, suffix = '') {
  return `/api/personas/${encodeURIComponent(personaId)}/memories${suffix}`;
}

export async function searchPersonaMemories(
  personaId: string,
  input: {
    query: string;
    category?: MemoryCategory;
    importance?: MemoryImportance;
    cursor?: string;
  },
  signal?: AbortSignal
): Promise<MemoryQueryPageReceipt> {
  const query = new URLSearchParams({ query: input.query });
  if (input.category) query.set('category', input.category);
  if (input.importance) query.set('importance', input.importance);
  if (input.cursor) query.set('cursor', input.cursor);
  return readJson<MemoryQueryPageReceipt>(
    await apiFetch(`${personaMemoryPath(personaId)}?${query.toString()}`, { signal })
  );
}

export async function fetchPersonaMemory(
  personaId: string,
  memoryId: string,
  signal?: AbortSignal
): Promise<MemoryDetailResponse> {
  return readJson<MemoryDetailResponse>(
    await apiFetch(personaMemoryPath(personaId, `/${encodeURIComponent(memoryId)}`), { signal })
  );
}

export async function fetchPersonaMemoryHistory(
  personaId: string,
  memoryId: string,
  signal?: AbortSignal
): Promise<MemoryHistoryResponse> {
  return readJson<MemoryHistoryResponse>(
    await apiFetch(
      personaMemoryPath(personaId, `/${encodeURIComponent(memoryId)}/history`),
      { signal }
    )
  );
}

export async function createPersonaMemory(
  personaId: string,
  input: MemoryCreateInput
): Promise<MemoryMutationReceipt> {
  return readJson<MemoryMutationReceipt>(
    await apiFetch(personaMemoryPath(personaId), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input)
    })
  );
}

export async function correctPersonaMemory(
  personaId: string,
  memoryId: string,
  input: MemoryCorrectInput
): Promise<MemoryMutationReceipt> {
  return readJson<MemoryMutationReceipt>(
    await apiFetch(personaMemoryPath(personaId, `/${encodeURIComponent(memoryId)}/correct`), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input)
    })
  );
}

export async function adjustPersonaMemoryImportance(
  personaId: string,
  memoryId: string,
  input: MemoryImportanceInput
): Promise<MemoryImportanceReceipt> {
  return readJson<MemoryImportanceReceipt>(
    await apiFetch(personaMemoryPath(personaId, `/${encodeURIComponent(memoryId)}/importance`), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input)
    })
  );
}

export async function deletePersonaMemory(
  personaId: string,
  memoryId: string,
  operationId: string
): Promise<MemoryDeleteReceipt> {
  return readJson<MemoryDeleteReceipt>(
    await apiFetch(personaMemoryPath(personaId, `/${encodeURIComponent(memoryId)}`), {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ operation_id: operationId })
    })
  );
}

export async function clearPersonaMemories(
  personaId: string,
  operationId: string
): Promise<MemoryDeleteReceipt> {
  return readJson<MemoryDeleteReceipt>(
    await apiFetch(personaMemoryPath(personaId), {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ operation_id: operationId })
    })
  );
}
