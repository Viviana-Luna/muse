import { Globe2, ShieldCheck } from 'lucide-react';

import { SecretInput } from './SettingsFields';
import type { SettingsDialogProps } from '../types';

type WebSearchPanelProps = Pick<
  SettingsDialogProps,
  | 'busy'
  | 'webSearchConfigured'
  | 'webSearchKeyDraft'
  | 'webSearchAction'
  | 'setWebSearchKeyDraft'
  | 'onDeleteWebSearchKey'
>;

export function WebSearchPanel({
  busy,
  webSearchConfigured,
  webSearchKeyDraft,
  webSearchAction,
  setWebSearchKeyDraft,
  onDeleteWebSearchKey
}: WebSearchPanelProps) {
  return (
    <div className="settings-panel-body">
      <section className="settings-module web-search-settings-card">
        <header className="settings-module-head">
          <div>
            <h2>搜索服务</h2>
            <p>管理 Brave Search API 凭据及当前启用状态。</p>
          </div>
        </header>
        <div className="web-search-provider wide">
          <span className="web-search-provider-icon" aria-hidden="true">
            <Globe2 />
          </span>
          <span>
            <strong>Brave Search API</strong>
            <small>仅在角色请求联网搜索且你批准后调用。</small>
          </span>
          <i className={webSearchConfigured && webSearchAction !== 'delete' ? 'configured' : ''}>
            {webSearchAction === 'delete'
              ? '待删除'
              : webSearchConfigured
                ? '已配置'
                : '未配置'}
          </i>
        </div>
        <label className="wide">
          Brave Search API Key
          <div className="provider-secret-row">
            <SecretInput
              configured={webSearchConfigured}
              value={webSearchKeyDraft}
              aria-label="Brave Search API Key"
              placeholder={webSearchConfigured ? '输入新密钥以替换当前凭据' : '输入 Brave Search API Key'}
              onChange={(event) => setWebSearchKeyDraft(event.target.value)}
            />
            {webSearchConfigured && webSearchAction !== 'delete' && (
              <button type="button" onClick={() => void onDeleteWebSearchKey()} disabled={busy}>
                标记删除
              </button>
            )}
          </div>
          <span className="field-hint">
            点击“保存全部更改”后才会写入系统凭据库；Muse 不会保存或回显明文。
          </span>
        </label>
      </section>

      <section className="settings-module settings-module-compact">
        <div className="web-search-safety-note wide">
          <ShieldCheck aria-hidden="true" />
          <span>
            <strong>联网仍需逐次批准</strong>
            <small>配置密钥只启用能力，不会绕过联网确认、HTTPS 限制或外部内容不可信标记。</small>
          </span>
        </div>
      </section>
    </div>
  );
}
