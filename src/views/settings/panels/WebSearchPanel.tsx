import { Globe2, KeyRound, ShieldCheck } from 'lucide-react';

import { SecretInput } from './SettingsFields';
import type { SettingsDialogProps } from '../types';

type WebSearchPanelProps = Pick<
  SettingsDialogProps,
  | 'busy'
  | 'webSearchProvider'
  | 'webSearchConfigured'
  | 'webSearchKeyDraft'
  | 'webSearchAction'
  | 'setWebSearchProvider'
  | 'setWebSearchKeyDraft'
  | 'onDeleteWebSearchKey'
>;

export function WebSearchPanel({
  busy,
  webSearchProvider,
  webSearchConfigured,
  webSearchKeyDraft,
  webSearchAction,
  setWebSearchProvider,
  setWebSearchKeyDraft,
  onDeleteWebSearchKey
}: WebSearchPanelProps) {
  return (
    <div className="settings-panel-body">
      <section className="settings-module web-search-settings-card">
        <header className="settings-module-head">
          <div>
            <h2>搜索服务</h2>
            <p>默认使用 Exa 免费搜索，也可以切换到自己的 API 额度。</p>
          </div>
        </header>
        <div className="web-search-provider-list wide" role="radiogroup" aria-label="Exa 搜索方案">
          <button
            type="button"
            className={`web-search-provider ${webSearchProvider === 'exa_free_mcp' ? 'active' : ''}`}
            role="radio"
            aria-checked={webSearchProvider === 'exa_free_mcp'}
            onClick={() => setWebSearchProvider('exa_free_mcp')}
            disabled={busy}
          >
            <span className="web-search-provider-icon" aria-hidden="true">
              <Globe2 />
            </span>
            <span>
              <strong>Exa 免费搜索</strong>
              <small>无需账号和密钥；达到公共限流后可稍后重试。</small>
            </span>
            <i className={webSearchProvider === 'exa_free_mcp' ? 'configured' : ''}>
              {webSearchProvider === 'exa_free_mcp' ? '使用中' : '默认方案'}
            </i>
          </button>
          <button
            type="button"
            className={`web-search-provider ${webSearchProvider === 'exa_api' ? 'active' : ''}`}
            role="radio"
            aria-checked={webSearchProvider === 'exa_api'}
            onClick={() => setWebSearchProvider('exa_api')}
            disabled={busy}
          >
            <span className="web-search-provider-icon" aria-hidden="true">
              <KeyRound />
            </span>
            <span>
              <strong>Exa API Key</strong>
              <small>使用自己的免费月度额度、独立限流和用量统计。</small>
            </span>
            <i
              className={
                webSearchConfigured && webSearchAction !== 'delete' ? 'configured' : ''
              }
            >
              {webSearchAction === 'delete'
                ? '待删除'
                : webSearchConfigured
                  ? webSearchProvider === 'exa_api'
                    ? '使用中'
                    : '已配置'
                  : '未配置'}
            </i>
          </button>
        </div>
        {webSearchProvider === 'exa_api' && (
          <label className="wide">
            Exa API Key
            <div className="provider-secret-row">
              <SecretInput
                configured={webSearchConfigured}
                value={webSearchKeyDraft}
                aria-label="Exa API Key"
                placeholder={webSearchConfigured ? '输入新密钥以替换当前凭据' : '输入 Exa API Key'}
                onChange={(event) => setWebSearchKeyDraft(event.target.value)}
              />
              {webSearchConfigured && webSearchAction !== 'delete' && (
                <button type="button" onClick={() => void onDeleteWebSearchKey()} disabled={busy}>
                  标记删除
                </button>
              )}
            </div>
            <span className="field-hint">
              点击“保存全部更改”后才会写入受保护的 config.toml；切回免费搜索不会删除已保存密钥。
            </span>
          </label>
        )}
      </section>

      <section className="settings-module settings-module-compact">
        <div className="web-search-safety-note wide">
          <ShieldCheck aria-hidden="true" />
          <span>
            <strong>联网仍受当前会话审批策略约束</strong>
            <small>切换搜索方案不会绕过审批、HTTPS 限制或外部内容不可信标记。</small>
          </span>
        </div>
      </section>
    </div>
  );
}
