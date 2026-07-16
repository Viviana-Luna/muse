import { useEffect, useState } from 'react';

import { apiErrorFromResponse, apiFetch } from '@/api/client';

export interface ResolvedAssetUrl {
  url: string;
  revoke: () => void;
}

function requiresAuthorization(path: string) {
  return path.startsWith('/api/');
}

export async function resolveAuthenticatedAssetUrl(
  path: string,
  signal?: AbortSignal
): Promise<ResolvedAssetUrl> {
  if (!requiresAuthorization(path)) {
    return { url: path, revoke: () => undefined };
  }
  const response = await apiFetch(path, { signal });
  if (!response.ok) {
    throw await apiErrorFromResponse(response, `无法读取受保护的角色资源（HTTP ${response.status}）。`);
  }
  const objectUrl = URL.createObjectURL(await response.blob());
  return {
    url: objectUrl,
    revoke: () => URL.revokeObjectURL(objectUrl)
  };
}

export function useAuthenticatedAssetUrl(path: string) {
  const [resolvedUrl, setResolvedUrl] = useState(() => (requiresAuthorization(path) ? '' : path));

  useEffect(() => {
    if (!requiresAuthorization(path)) {
      setResolvedUrl(path);
      return undefined;
    }
    const controller = new AbortController();
    let resolved: ResolvedAssetUrl | null = null;
    setResolvedUrl('');
    void resolveAuthenticatedAssetUrl(path, controller.signal)
      .then((next) => {
        if (controller.signal.aborted) {
          next.revoke();
          return;
        }
        resolved = next;
        setResolvedUrl(next.url);
      })
      .catch(() => {
        if (!controller.signal.aborted) setResolvedUrl('');
      });
    return () => {
      controller.abort();
      resolved?.revoke();
    };
  }, [path]);

  return resolvedUrl;
}
