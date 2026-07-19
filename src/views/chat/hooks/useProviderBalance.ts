// 当前聊天供应商余额，用于标题栏上下文详情展示。

import { useCallback, useEffect, useRef, useState } from 'react';

import { fetchModelsConfig, fetchProviderBalance } from '@/api';

// 用量刷新做防抖，避免一轮对话的多次用量事件触发重复查询。
const USAGE_REFRESH_DEBOUNCE_MS = 2000;

/**
 * 查询当前聊天供应商的账户余额，返回可直接展示的文本（如 `CNY 12.3`）。
 *
 * 余额接口需要 provider 与 api_base，来自后端运行时模型配置；
 * API Key 由后端回落到已保存凭据。供应商不支持或查询失败时返回 `null`，
 * 由展示层决定不渲染余额行。
 *
 * 刷新时机：当前模型变化（含启动后的首次读取）与对话用量更新（防抖）。
 */
export function useProviderBalance(modelLabel: string, usageItemCount: number): string | null {
  const [balanceLabel, setBalanceLabel] = useState<string | null>(null);
  const requestRef = useRef(0);

  const query = useCallback(async () => {
    const requestId = ++requestRef.current;
    try {
      const config = await fetchModelsConfig();
      const provider = config.chat.provider.trim();
      const apiBase = config.chat.api_base.trim();
      if (!provider || !apiBase) {
        if (requestRef.current === requestId) setBalanceLabel(null);
        return;
      }
      const balance = await fetchProviderBalance({
        provider,
        api_base: apiBase,
        purpose: 'chat',
        api_key: null
      });
      if (requestRef.current !== requestId) return;
      const label = balance.balance_infos
        .map((item) => `${item.currency} ${item.total_balance}`)
        .filter((text) => text.trim().length > 0)
        .join(' · ');
      setBalanceLabel(label || null);
    } catch {
      if (requestRef.current === requestId) setBalanceLabel(null);
    }
  }, []);

  useEffect(() => {
    void query();
  }, [modelLabel, query]);

  useEffect(() => {
    if (usageItemCount === 0) return;
    const timer = window.setTimeout(() => void query(), USAGE_REFRESH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [usageItemCount, query]);

  return balanceLabel;
}
