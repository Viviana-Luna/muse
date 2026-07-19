import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  fetchModelsConfig: vi.fn(),
  fetchProviderBalance: vi.fn()
}));

vi.mock('@/api', () => api);

import { useProviderBalance } from './useProviderBalance';

function mockChatConfig(provider = 'deepseek', apiBase = 'https://api.deepseek.com') {
  api.fetchModelsConfig.mockResolvedValue({
    chat: { provider, api_base: apiBase }
  });
}

function mockBalance(balanceInfos: Array<Record<string, string>>) {
  api.fetchProviderBalance.mockResolvedValue({
    provider_id: 'deepseek',
    is_available: true,
    balance_infos: balanceInfos,
    status: '供应商余额可用。'
  });
}

describe('useProviderBalance', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockChatConfig();
    mockBalance([
      {
        currency: 'CNY',
        total_balance: '66.23',
        granted_balance: '0.00',
        topped_up_balance: '66.23'
      }
    ]);
  });

  afterEach(() => {
    cleanup();
  });

  it('查询当前聊天供应商余额并拼接多币种展示文本', async () => {
    mockBalance([
      { currency: 'CNY', total_balance: '66.23' },
      { currency: 'USD', total_balance: '0.10' }
    ]);
    const { result } = renderHook(() => useProviderBalance('DeepSeek / deepseek-chat', 0));

    await waitFor(() => expect(result.current).toBe('CNY 66.23 · USD 0.10'));
    expect(api.fetchProviderBalance).toHaveBeenCalledWith({
      provider: 'deepseek',
      api_base: 'https://api.deepseek.com',
      purpose: 'chat',
      api_key: null
    });
  });

  it('供应商不支持或查询失败时静默降级为 null', async () => {
    api.fetchProviderBalance.mockRejectedValue(new Error('当前提供商暂未实现余额检测。'));
    const { result } = renderHook(() => useProviderBalance('自定义 / foo', 0));

    await waitFor(() => expect(api.fetchProviderBalance).toHaveBeenCalled());
    expect(result.current).toBeNull();
  });

  it('聊天供应商或 API Base 未配置时不发起余额查询', async () => {
    mockChatConfig('', '');
    const { result } = renderHook(() => useProviderBalance('模型未配置', 0));

    await waitFor(() => expect(api.fetchModelsConfig).toHaveBeenCalled());
    expect(api.fetchProviderBalance).not.toHaveBeenCalled();
    expect(result.current).toBeNull();
  });

  it('模型变化时重新查询余额', async () => {
    const { result, rerender } = renderHook(
      ({ modelLabel }) => useProviderBalance(modelLabel, 0),
      { initialProps: { modelLabel: 'DeepSeek / deepseek-chat' } }
    );

    await waitFor(() => expect(result.current).toBe('CNY 66.23'));
    rerender({ modelLabel: 'DeepSeek / deepseek-v4-pro' });
    await waitFor(() => expect(api.fetchProviderBalance).toHaveBeenCalledTimes(2));
  });

  it('对话用量更新后防抖刷新余额', async () => {
    vi.useFakeTimers();
    try {
      const { rerender } = renderHook(
        ({ usageItemCount }) => useProviderBalance('DeepSeek / deepseek-chat', usageItemCount),
        { initialProps: { usageItemCount: 0 } }
      );
      await act(async () => {});
      expect(api.fetchProviderBalance).toHaveBeenCalledTimes(1);

      rerender({ usageItemCount: 1 });
      rerender({ usageItemCount: 2 });
      await act(async () => {
        vi.advanceTimersByTime(2100);
      });
      expect(api.fetchProviderBalance).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });
});
