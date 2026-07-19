import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { WebSearchPanel } from './WebSearchPanel';

describe('WebSearchPanel', () => {
  afterEach(() => {
    cleanup();
  });

  it('默认选择 Exa 免费搜索，切换 API 模式后才显示密钥输入框', () => {
    const setWebSearchProvider = vi.fn();
    const { rerender } = render(
      <WebSearchPanel
        busy={false}
        webSearchProvider="exa_free_mcp"
        webSearchConfigured={false}
        webSearchKeyDraft=""
        webSearchAction="keep"
        setWebSearchProvider={setWebSearchProvider}
        setWebSearchKeyDraft={vi.fn()}
        onDeleteWebSearchKey={vi.fn()}
      />
    );

    expect(screen.getByRole('radio', { name: /Exa 免费搜索/ })).toHaveAttribute(
      'aria-checked',
      'true'
    );
    expect(screen.queryByLabelText('Exa API Key')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('radio', { name: /Exa API Key/ }));
    expect(setWebSearchProvider).toHaveBeenCalledWith('exa_api');

    const setWebSearchKeyDraft = vi.fn();
    rerender(
      <WebSearchPanel
        busy={false}
        webSearchProvider="exa_api"
        webSearchConfigured={false}
        webSearchKeyDraft=""
        webSearchAction="keep"
        setWebSearchProvider={setWebSearchProvider}
        setWebSearchKeyDraft={setWebSearchKeyDraft}
        onDeleteWebSearchKey={vi.fn()}
      />
    );

    expect(screen.getByText('未配置')).toBeVisible();
    fireEvent.change(screen.getByLabelText('Exa API Key'), {
      target: { value: 'exa-test-key' }
    });
    expect(setWebSearchKeyDraft).toHaveBeenCalledWith('exa-test-key');
  });

  it('已配置时仍允许输入新密钥替换当前凭据', () => {
    const onDeleteWebSearchKey = vi.fn();
    render(
      <WebSearchPanel
        busy={false}
        webSearchProvider="exa_api"
        webSearchConfigured
        webSearchKeyDraft="exa-replace-key"
        webSearchAction="replace"
        setWebSearchProvider={vi.fn()}
        setWebSearchKeyDraft={vi.fn()}
        onDeleteWebSearchKey={onDeleteWebSearchKey}
      />
    );

    expect(screen.getByText('已配置，可留空保持')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '标记删除' }));
    expect(onDeleteWebSearchKey).toHaveBeenCalledOnce();
  });
});
