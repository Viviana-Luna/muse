import { describe, expect, it } from 'vitest';

import { formatExactTokenCount, formatTokenCount } from './tokenActivity';

describe('tokenActivity', () => {
  it('压缩展示大 Token 计数', () => {
    expect(formatTokenCount(9_999)).toBe('9999');
    expect(formatTokenCount(87_053)).toBe('87.1K');
    expect(formatTokenCount(1_240_000)).toBe('1.2M');
  });

  it('完整展示带千分位的精确 Token 计数', () => {
    expect(formatExactTokenCount(87_053)).toBe('87,053');
    expect(formatExactTokenCount(128)).toBe('128');
  });
});
