import { invoke, isTauri } from '@tauri-apps/api/core';

export interface RuntimeBootstrap {
  api_origin: string;
  access_token: string;
  token_type: 'Bearer' | string;
  protocol_version: string;
  instance_id: string;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly fieldErrors: Record<string, string>;
  readonly retryable: boolean;
  readonly requestId?: string;
  readonly turnId?: string;
  readonly phase?: string;

  constructor(
    message: string,
    status: number,
    details: {
      code?: string;
      fieldErrors?: Record<string, string>;
      retryable?: boolean;
      requestId?: string;
      turnId?: string;
      phase?: string;
    } = {}
  ) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = details.code;
    this.fieldErrors = details.fieldErrors ?? {};
    this.retryable = details.retryable ?? false;
    this.requestId = details.requestId;
    this.turnId = details.turnId;
    this.phase = details.phase;
  }
}

/** 将 API 错误转换为可直接展示的安全诊断文本，不包含响应正文、请求头或密钥。 */
export function formatApiErrorMessage(error: unknown, fallback = '请求失败。'): string {
  if (!(error instanceof ApiError)) {
    return error instanceof Error && error.message.trim() ? error.message : fallback;
  }

  const details = [
    error.code ? `错误码：${error.code}` : '',
    error.status > 0 ? `HTTP ${error.status}` : '',
    Object.keys(error.fieldErrors).length > 0
      ? `字段：${Object.keys(error.fieldErrors).join('、')}`
      : '',
    error.requestId ? `请求 ID：${error.requestId}` : '',
    error.turnId ? `回合 ID：${error.turnId}` : '',
    error.phase ? `阶段：${error.phase}` : '',
    error.retryable ? '可以稍后重试' : ''
  ].filter(Boolean);
  return details.length > 0
    ? `${error.message}\n${details.join(' · ')}`
    : error.message || fallback;
}

let bootstrapPromise: Promise<RuntimeBootstrap | null> | null = null;
const SUPPORTED_PROTOCOL_VERSION = 'muse-local-api/v1';

function normalizeApiOrigin(value: string) {
  return value.trim().replace(/\/+$/u, '');
}

function runtimeBootstrap(): Promise<RuntimeBootstrap | null> {
  if (!bootstrapPromise) {
    bootstrapPromise = isTauri()
      ? invoke<RuntimeBootstrap>('runtime_bootstrap').then((bootstrap) => {
          if (!bootstrap.api_origin?.trim() || !bootstrap.access_token?.trim()) {
            throw new Error('桌面运行时未返回有效的 API 地址或访问令牌。');
          }
          if (bootstrap.token_type.toLowerCase() !== 'bearer') {
            throw new Error(`桌面运行时返回了不支持的令牌类型：${bootstrap.token_type}`);
          }
          if (bootstrap.protocol_version !== SUPPORTED_PROTOCOL_VERSION) {
            throw new Error(`桌面运行时协议版本不兼容：${bootstrap.protocol_version}`);
          }
          return {
            ...bootstrap,
            api_origin: normalizeApiOrigin(bootstrap.api_origin),
            access_token: bootstrap.access_token.trim()
          };
        })
      : Promise.resolve(null);
  }
  return bootstrapPromise;
}

function resolveApiUrl(input: RequestInfo | URL, apiOrigin: string): RequestInfo | URL {
  if (!apiOrigin || typeof input !== 'string' || !input.startsWith('/')) return input;
  return `${apiOrigin}${input}`;
}

function targetsApiOrigin(input: RequestInfo | URL, apiOrigin: string) {
  if (!apiOrigin) return false;
  if (typeof input === 'string' && input.startsWith('/')) return true;
  const value =
    typeof input === 'string'
      ? input
      : input instanceof URL
        ? input.href
        : input.url;
  try {
    return new URL(value).origin === new URL(apiOrigin).origin;
  } catch {
    return false;
  }
}

// 所有领域 API 都通过此入口发起请求。桌面端先向 Tauri 宿主取得短生命周期内存凭据，
// 普通浏览器开发环境则保留同源请求，不在 localStorage 或 URL 中持久化令牌。
export async function apiFetch(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
  const bootstrap = await runtimeBootstrap();
  const headers = new Headers(init.headers);
  if (
    bootstrap?.access_token &&
    targetsApiOrigin(input, bootstrap.api_origin) &&
    !headers.has('Authorization')
  ) {
    headers.set('Authorization', `Bearer ${bootstrap.access_token}`);
  }
  return fetch(resolveApiUrl(input, bootstrap?.api_origin ?? ''), {
    ...init,
    headers
  });
}

/// 原生桌面内嵌页完成 Bootstrap 与鉴权 health 后，向宿主提交无密钥就绪证明。
/// 浏览器开发环境不执行该协议。
export async function reportDesktopReady(): Promise<void> {
  if (!isTauri()) return;
  const bootstrap = await runtimeBootstrap();
  if (!bootstrap) throw new Error('桌面运行时 Bootstrap 不可用。');
  const response = await apiFetch('/api/runtime/health');
  const health = (await response.json().catch(() => ({}))) as {
    status?: string;
    protocol_version?: string;
    instance_id?: string;
  };
  if (
    !response.ok ||
    health.status !== 'ok' ||
    health.protocol_version !== bootstrap.protocol_version ||
    health.instance_id !== bootstrap.instance_id
  ) {
    throw new Error('桌面本地 API 健康状态与 Bootstrap 不一致。');
  }
  await invoke('runtime_ready', {
    protocolVersion: bootstrap.protocol_version,
    instanceId: bootstrap.instance_id
  });
}

export async function readJson<T>(response: Response): Promise<T> {
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw apiErrorFromPayload(payload, response.status, `请求失败（HTTP ${response.status}）`);
  }
  return payload as T;
}

export async function apiErrorFromResponse(response: Response, fallback: string): Promise<ApiError> {
  const payload = await response.json().catch(() => ({}));
  return apiErrorFromPayload(payload, response.status, fallback);
}

function apiErrorFromPayload(payload: any, status: number, fallback: string): ApiError {
  const nested = payload?.error && typeof payload.error === 'object' ? payload.error : undefined;
  const message =
    typeof payload?.message === 'string'
      ? payload.message
      : typeof payload?.error === 'string'
        ? payload.error
        : typeof nested?.message === 'string'
          ? nested.message
          : fallback;
  const code =
    typeof payload?.code === 'string'
      ? payload.code
      : typeof nested?.code === 'string'
        ? nested.code
        : undefined;
  const fieldErrors =
    payload?.field_errors && typeof payload.field_errors === 'object'
      ? (payload.field_errors as Record<string, string>)
      : {};
  return new ApiError(message, status, {
    code,
    fieldErrors,
    retryable: payload?.retryable === true,
    requestId: typeof payload?.request_id === 'string' ? payload.request_id : undefined,
    turnId: typeof payload?.turn_id === 'string' ? payload.turn_id : undefined,
    phase: typeof payload?.phase === 'string' ? payload.phase : undefined
  });
}
