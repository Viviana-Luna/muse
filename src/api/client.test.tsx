import { beforeEach, describe, expect, it, vi } from 'vitest';

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(() => true)
}));

vi.mock('@tauri-apps/api/core', () => tauri);

import { ApiError, apiFetch, formatApiErrorMessage } from './client';

describe('apiFetch', () => {
  beforeEach(() => {
    tauri.invoke.mockResolvedValue({
      api_origin: 'http://127.0.0.1:43127/',
      access_token: 'runtime-secret',
      token_type: 'Bearer',
      protocol_version: 'muse-api/v1',
      instance_id: 'instance-1'
    });
  });

  it('通过 Tauri bootstrap 解析 API 地址并注入 Bearer', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}'));

    await apiFetch('/api/models');

    expect(tauri.invoke).toHaveBeenCalledWith('runtime_bootstrap');
    expect(fetchMock).toHaveBeenCalledOnce();
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe('http://127.0.0.1:43127/api/models');
    expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer runtime-secret');
    fetchMock.mockRestore();
  });

  it('不会把桌面访问令牌发送到运行时之外的地址', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}'));

    await apiFetch('https://example.com/public.json');

    const [, init] = fetchMock.mock.calls[0];
    expect(new Headers(init?.headers).has('Authorization')).toBe(false);
    fetchMock.mockRestore();
  });

  it('格式化保存失败的稳定诊断字段但不暴露响应正文', () => {
    const message = formatApiErrorMessage(
      new ApiError('上游模型服务拒绝了请求。', 502, {
        code: 'provider_invalid_request',
        requestId: 'req-42',
        fieldErrors: { model: '模型无效' },
        retryable: false
      })
    );

    expect(message).toContain('上游模型服务拒绝了请求。');
    expect(message).toContain('错误码：provider_invalid_request');
    expect(message).toContain('HTTP 502');
    expect(message).toContain('字段：model');
    expect(message).toContain('请求 ID：req-42');
    expect(message).not.toContain('模型无效');
  });
});
