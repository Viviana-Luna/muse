import type { RuntimeTokenUsage } from '@/types';

// Token 主数据源统一来自后端 runtime usage；前端不再估算或写入 localStorage。
export function formatTokenCount(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 10_000) return `${(value / 1_000).toFixed(1)}K`;
  return Math.round(value).toString();
}

export function formatExactTokenCount(value: number): string {
  return new Intl.NumberFormat('en-US', { maximumFractionDigits: 0 }).format(value);
}

interface DeepSeekUsdPricing {
  cacheHitInput: number;
  cacheMissInput: number;
  output: number;
}

// 费率单位为 USD / 1M Token，集中维护以便 DeepSeek 调价时单点更新。
const DEEPSEEK_USD_PRICING: Record<string, DeepSeekUsdPricing> = {
  'deepseek-v4-flash': {
    cacheHitInput: 0.0028,
    cacheMissInput: 0.14,
    output: 0.28
  },
  'deepseek-v4-pro': {
    cacheHitInput: 0.003625,
    cacheMissInput: 0.435,
    output: 0.87
  }
};

function resolveDeepSeekPricing(model: string): DeepSeekUsdPricing | null {
  const normalized = model.trim().toLowerCase();
  if (normalized === 'deepseek-chat' || normalized === 'deepseek-reasoner') {
    return DEEPSEEK_USD_PRICING['deepseek-v4-flash'];
  }
  return DEEPSEEK_USD_PRICING[normalized] ?? null;
}

/**
 * 计算当前查询范围内的 DeepSeek API 理论成本。
 *
 * `input_tokens` 已由后端规范化为缓存未命中部分，`cache_read_input_tokens`
 * 是缓存命中部分；reasoning token 已包含在模型返回的 output token 中，不重复计费。
 */
export function calculateDeepSeekUsdCost(items: RuntimeTokenUsage[]): number | null {
  const deepSeekItems = items.filter((item) => item.provider.trim().toLowerCase() === 'deepseek');
  if (deepSeekItems.length === 0) return 0;

  let total = 0;
  for (const item of deepSeekItems) {
    const pricing = resolveDeepSeekPricing(item.model);
    if (!pricing) return null;
    const cacheMissTokens = item.input_tokens + item.cache_creation_input_tokens;
    total +=
      (cacheMissTokens * pricing.cacheMissInput +
        item.cache_read_input_tokens * pricing.cacheHitInput +
        item.output_tokens * pricing.output) /
      1_000_000;
  }
  return total;
}

export function formatUsdCost(value: number | null): string {
  if (value === null) return '--';
  const fractionDigits = value > 0 && value < 0.01 ? 4 : 2;
  return `US$${value.toFixed(fractionDigits)}`;
}
