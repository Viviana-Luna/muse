import { describe, expect, it } from 'vitest';

import type { RuntimeTokenUsage } from '@/types';

import {
  calculateDeepSeekUsdCost,
  formatExactTokenCount,
  formatUsdCost
} from './tokenActivity';

function usage(overrides: Partial<RuntimeTokenUsage> = {}): RuntimeTokenUsage {
  return {
    id: 'usage-1',
    conversation_id: 'conversation-1',
    turn_id: 'turn-1',
    provider: 'deepseek',
    model: 'deepseek-v4-flash',
    created_at: '2026-07-15T00:00:00Z',
    input_tokens: 1_000_000,
    output_tokens: 1_000_000,
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: 1_000_000,
    reasoning_tokens: 0,
    server_tool_tokens: 0,
    total_tokens: 3_000_000,
    source: 'provider_reported',
    ...overrides
  };
}

describe('tokenActivity', () => {
  it('按 Flash 的缓存命中、未命中与输出费率分别计算成本', () => {
    expect(calculateDeepSeekUsdCost([usage()])).toBeCloseTo(0.4228, 8);
    expect(calculateDeepSeekUsdCost([usage({ model: 'deepseek-chat' })])).toBeCloseTo(
      0.4228,
      8
    );
  });

  it('按 Pro 费率计算并拒绝未知 DeepSeek 模型的伪精确成本', () => {
    expect(calculateDeepSeekUsdCost([usage({ model: 'deepseek-v4-pro' })])).toBeCloseTo(
      1.308625,
      8
    );
    expect(calculateDeepSeekUsdCost([usage({ model: 'custom-deepseek-model' })])).toBeNull();
  });

  it('忽略其他供应商，并使用适合紧凑提示层的格式', () => {
    expect(calculateDeepSeekUsdCost([usage({ provider: 'volcengine_agent_plan' })])).toBe(0);
    expect(formatExactTokenCount(87_053)).toBe('87,053');
    expect(formatUsdCost(0)).toBe('US$0.00');
    expect(formatUsdCost(0.004321)).toBe('US$0.0043');
    expect(formatUsdCost(null)).toBe('--');
  });
});
