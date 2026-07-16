import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { WebSearchPanel } from './WebSearchPanel';

describe('WebSearchPanel', () => {
  afterEach(() => {
    cleanup();
  });

  it('明确展示 Brave 密钥状态，并把新密钥保留为统一保存草稿', () => {
    const setWebSearchKeyDraft = vi.fn();
    render(
      <WebSearchPanel
        busy={false}
        webSearchConfigured={false}
        webSearchKeyDraft=""
        webSearchAction="keep"
        setWebSearchKeyDraft={setWebSearchKeyDraft}
        onDeleteWebSearchKey={vi.fn()}
      />
    );

    expect(screen.getByText('未配置')).toBeVisible();
    fireEvent.change(screen.getByLabelText('Brave Search API Key'), {
      target: { value: 'brv-test-key' }
    });
    expect(setWebSearchKeyDraft).toHaveBeenCalledWith('brv-test-key');
  });

  it('已配置时仍允许输入新密钥替换当前凭据', () => {
    const onDeleteWebSearchKey = vi.fn();
    render(
      <WebSearchPanel
        busy={false}
        webSearchConfigured
        webSearchKeyDraft="brv-replace-key"
        webSearchAction="replace"
        setWebSearchKeyDraft={vi.fn()}
        onDeleteWebSearchKey={onDeleteWebSearchKey}
      />
    );

    expect(screen.getByText('已配置')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '标记删除' }));
    expect(onDeleteWebSearchKey).toHaveBeenCalledOnce();
  });
});
