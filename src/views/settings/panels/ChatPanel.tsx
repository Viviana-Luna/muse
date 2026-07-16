import { useEffect, useMemo, useState } from 'react';
import {
  Database,
  KeyRound,
  Pencil,
  Plus,
  Server,
  ShieldCheck,
  Trash2,
  X
} from 'lucide-react';

import { useModalAccessibility } from '@/hooks/useModalAccessibility';
import type { ModelCatalogItem, ModelCatalogMutation } from '@/types';
import { SecretInput } from './SettingsFields';
import type { SettingsDialogProps } from '../types';

type ChatPanelProps = Pick<
  SettingsDialogProps,
  | 'settingsConfig'
  | 'modelCatalog'
  | 'busy'
  | 'onSaveProviderCredential'
  | 'onDeleteProviderCredential'
  | 'onVerifyCatalogProvider'
  | 'onCreateCatalogModel'
  | 'onUpdateCatalogModel'
  | 'onDeleteCatalogModel'
  | 'capabilityBadges'
>;

interface ModelEditorState {
  mode: 'create' | 'edit';
  draft: ModelCatalogMutation;
}

function emptyModelDraft(providerId: string): ModelCatalogMutation {
  return {
    provider_id: providerId,
    model: '',
    name: '',
    notes: '',
    tags: ['reasoning', 'tool'],
    functions: ['chat'],
    context_window: 200_000,
    default_max_output_tokens: 2_048,
    supports_usage: true,
    supports_cached_tokens: false,
    supports_reasoning_tokens: true,
    tokenizer_family: 'rough_estimate'
  };
}

function modelDraftFromItem(model: ModelCatalogItem): ModelCatalogMutation {
  return {
    provider_id: model.provider_id,
    model: model.model,
    name: model.name,
    notes: model.notes,
    tags: model.tags,
    functions: model.functions,
    context_window: model.context_window,
    default_max_output_tokens: model.default_max_output_tokens,
    supports_usage: model.supports_usage,
    supports_cached_tokens: model.supports_cached_tokens,
    supports_reasoning_tokens: model.supports_reasoning_tokens,
    tokenizer_family: model.tokenizer_family
  };
}

export function ChatPanel(props: ChatPanelProps) {
  const {
    settingsConfig,
    modelCatalog,
    busy,
    onSaveProviderCredential,
    onDeleteProviderCredential,
    onVerifyCatalogProvider,
    onCreateCatalogModel,
    onUpdateCatalogModel,
    onDeleteCatalogModel,
    capabilityBadges
  } = props;
  const providers = useMemo(
    () =>
      (modelCatalog?.providers ?? []).filter(
        (provider) => provider.enabled && provider.capabilities.includes('chat')
      ),
    [modelCatalog]
  );
  const [selectedProviderId, setSelectedProviderId] = useState(settingsConfig.chat.provider);
  const [credentialDraft, setCredentialDraft] = useState('');
  const [credentialDeleteArmed, setCredentialDeleteArmed] = useState(false);
  const [editor, setEditor] = useState<ModelEditorState | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ModelCatalogItem | null>(null);
  const editorModalRef = useModalAccessibility<HTMLElement>(Boolean(editor), () => setEditor(null));
  const deleteModalRef = useModalAccessibility<HTMLElement>(Boolean(deleteTarget), () =>
    setDeleteTarget(null)
  );

  useEffect(() => {
    if (providers.some((provider) => provider.id === selectedProviderId)) return;
    setSelectedProviderId(
      providers.find((provider) => provider.id === settingsConfig.chat.provider)?.id ??
        providers[0]?.id ??
        ''
    );
  }, [providers, selectedProviderId, settingsConfig.chat.provider]);

  useEffect(() => {
    setCredentialDraft('');
    setCredentialDeleteArmed(false);
  }, [selectedProviderId]);

  const selectedProvider = providers.find((provider) => provider.id === selectedProviderId);
  const models = (modelCatalog?.models ?? []).filter(
    (model) =>
      model.enabled &&
      model.provider_id === selectedProviderId &&
      model.functions.includes('chat')
  );
  const activeModelKey = `${settingsConfig.chat.provider}:${settingsConfig.chat.model}`;
  const verificationModel =
    selectedProviderId === settingsConfig.chat.provider
      ? models.find((model) => model.model === settingsConfig.chat.model)?.model ?? models[0]?.model
      : models[0]?.model;

  function beginCreateModel() {
    if (!selectedProviderId) return;
    setEditor({ mode: 'create', draft: emptyModelDraft(selectedProviderId) });
  }

  function beginEditModel(model: ModelCatalogItem) {
    setEditor({ mode: 'edit', draft: modelDraftFromItem(model) });
  }

  function updateEditor<K extends keyof ModelCatalogMutation>(
    key: K,
    value: ModelCatalogMutation[K]
  ) {
    setEditor((current) =>
      current
        ? { ...current, draft: { ...current.draft, [key]: value } }
        : current
    );
  }

  function toggleEditorTag(tag: string, enabled: boolean) {
    if (!editor) return;
    const tags = enabled
      ? Array.from(new Set([...editor.draft.tags, tag]))
      : editor.draft.tags.filter((item) => item !== tag);
    updateEditor('tags', tags);
  }

  async function submitEditor() {
    if (!editor) return;
    const draft = {
      ...editor.draft,
      model: editor.draft.model.trim(),
      name: editor.draft.name.trim(),
      notes: editor.draft.notes.trim()
    };
    if (!draft.provider_id || !draft.model || !draft.name) return;
    try {
      if (editor.mode === 'create') await onCreateCatalogModel(draft);
      else await onUpdateCatalogModel(draft);
      setEditor(null);
    } catch {
      // 控制器已经展示去敏后的详细错误；保留当前草稿供用户修正。
    }
  }

  async function saveCredential() {
    try {
      await onSaveProviderCredential(selectedProviderId, credentialDraft);
      setCredentialDraft('');
    } catch {
      // 保存失败时保留用户刚输入的密钥草稿，不写入页面或日志。
    }
  }

  async function deleteCredential() {
    if (!credentialDeleteArmed) {
      setCredentialDeleteArmed(true);
      return;
    }
    try {
      await onDeleteProviderCredential(selectedProviderId);
      setCredentialDeleteArmed(false);
    } catch {
      // 失败详情已由控制器展示，保持二次确认状态。
    }
  }

  async function confirmDeleteModel() {
    if (!deleteTarget) return;
    try {
      await onDeleteCatalogModel(deleteTarget.provider_id, deleteTarget.model);
      setDeleteTarget(null);
    } catch {
      // 活动模型冲突等错误由控制器展示，保留确认窗口。
    }
  }

  async function verifyProvider() {
    if (!selectedProvider) return;
    try {
      await onVerifyCatalogProvider(selectedProvider.id, verificationModel);
    } catch {
      // 连接错误已经以可读通知展示，页面保持当前供应商和模型选择。
    }
  }

  return (
    <>
      <section className="provider-settings-workspace" aria-label="模型配置">
        <aside className="provider-directory-column">
          <header className="provider-directory-head">
            <span>
              <small>模型提供商</small>
              <strong>{providers.length} 个连接</strong>
            </span>
          </header>
          <div className="provider-library-list" role="listbox" aria-label="选择供应商">
            {providers.map((provider) => (
              <button
                type="button"
                role="option"
                aria-selected={provider.id === selectedProviderId}
                className={provider.id === selectedProviderId ? 'active' : ''}
                key={provider.id}
                onClick={() => setSelectedProviderId(provider.id)}
              >
                <span className="provider-library-copy">
                  <strong>{provider.name}</strong>
                  <small>{provider.api_key_configured ? '凭据已配置' : '等待 API Key'}</small>
                </span>
              </button>
            ))}
          </div>
        </aside>

        <section className="provider-detail-column">
          {selectedProvider ? (
            <>
              <header className="provider-detail-head">
                <div>
                  <span>
                    <strong>{selectedProvider.name}</strong>
                    <small>{selectedProvider.notes || '对话模型供应商连接'}</small>
                  </span>
                </div>
                <i className={`provider-detail-status${selectedProvider.api_key_configured ? ' ready' : ''}`}>
                  {selectedProvider.api_key_configured ? '已配置' : '未配置'}
                </i>
              </header>

              <div className="provider-detail-scroll">
                <section className="provider-config-section" aria-labelledby="provider-secret-title">
                  <header>
                    <span>
                      <small>连接凭据</small>
                      <h2 id="provider-secret-title">API 密钥</h2>
                    </span>
                    <KeyRound aria-hidden="true" />
                  </header>
                  <label className="provider-field-label" htmlFor="provider-api-key">API Key</label>
                  <SecretInput
                    id="provider-api-key"
                    configured={selectedProvider.api_key_configured}
                    value={credentialDraft}
                    placeholder={selectedProvider.api_key_configured ? '输入新密钥以替换' : '输入 API Key'}
                    disabled={busy}
                    onChange={(event) => {
                      setCredentialDraft(event.target.value);
                      setCredentialDeleteArmed(false);
                    }}
                  />
                  <div className="provider-credential-actions">
                    <button
                      type="button"
                      className="primary"
                      disabled={busy || !credentialDraft.trim()}
                      onClick={() => void saveCredential()}
                    >
                      <KeyRound aria-hidden="true" />
                      保存凭据
                    </button>
                    <button
                      type="button"
                      disabled={busy || !selectedProvider.api_key_configured}
                      onClick={() => void verifyProvider()}
                    >
                      <ShieldCheck aria-hidden="true" />
                      验证连接
                    </button>
                    {selectedProvider.api_key_configured && (
                      <button
                        type="button"
                        className={credentialDeleteArmed ? 'danger' : 'quiet'}
                        disabled={busy}
                        onClick={() => void deleteCredential()}
                      >
                        <Trash2 aria-hidden="true" />
                        {credentialDeleteArmed ? '再次点击确认' : '删除凭据'}
                      </button>
                    )}
                  </div>
                </section>

                <section className="provider-config-section" aria-labelledby="provider-endpoint-title">
                  <header>
                    <span>
                      <small>请求入口</small>
                      <h2 id="provider-endpoint-title">API 地址</h2>
                    </span>
                    <Server aria-hidden="true" />
                  </header>
                  <label className="provider-field-label" htmlFor="provider-api-endpoint">API Base</label>
                  <input
                    id="provider-api-endpoint"
                    className="provider-endpoint-input"
                    value={selectedProvider.default_api_base}
                    readOnly
                  />
                  <p className="provider-field-hint">内置供应商地址由 config.toml Provider Profile 统一维护。</p>
                </section>

                <section className="provider-model-section" aria-labelledby="provider-model-list-title">
                  <header className="provider-model-section-head">
                    <span>
                      <small>模型目录</small>
                      <h2 id="provider-model-list-title">模型 <em>{models.length}</em></h2>
                    </span>
                    <button
                      type="button"
                      className="primary compact"
                      onClick={beginCreateModel}
                      disabled={busy || !selectedProviderId}
                    >
                      <Plus aria-hidden="true" />
                      新增模型
                    </button>
                  </header>
                  <div className="managed-model-list">
                    {models.map((model) => {
                      const active = `${model.provider_id}:${model.model}` === activeModelKey;
                      return (
                        <article className={active ? 'active' : ''} key={model.id}>
                          <div className="managed-model-main">
                            <span>
                              <strong>{model.name}</strong>
                              {active && <em>当前对话</em>}
                            </span>
                            <code title={model.model}>{model.model}</code>
                            {capabilityBadges(model)}
                          </div>
                          <dl>
                            <div>
                              <dt>上下文</dt>
                              <dd>{model.context_window.toLocaleString()}</dd>
                            </div>
                            <div>
                              <dt>默认输出</dt>
                              <dd>{model.default_max_output_tokens.toLocaleString()}</dd>
                            </div>
                          </dl>
                          <div className="managed-model-actions">
                            <button type="button" disabled={busy} onClick={() => beginEditModel(model)}>
                              <Pencil aria-hidden="true" />
                              编辑
                            </button>
                            <button
                              type="button"
                              className="danger quiet"
                              disabled={busy}
                              onClick={() => setDeleteTarget(model)}
                            >
                              <Trash2 aria-hidden="true" />
                              删除
                            </button>
                          </div>
                        </article>
                      );
                    })}
                    {models.length === 0 && (
                      <div className="managed-model-empty">
                        <Database aria-hidden="true" />
                        <strong>还没有模型</strong>
                        <p>为 {selectedProvider.name} 新增一个模型后，它会出现在聊天页选择器中。</p>
                        <button type="button" className="primary" onClick={beginCreateModel} disabled={busy}>
                          <Plus aria-hidden="true" />
                          新增模型
                        </button>
                      </div>
                    )}
                  </div>
                </section>

                <footer className="model-library-future-note">
                  自定义供应商与第二种 OpenAI-compatible 协议将在协议边界明确后开放；当前仅管理
                  DeepSeek 与火山方舟 Agent Plan。
                </footer>
              </div>
            </>
          ) : (
            <div className="provider-detail-scroll">
              <div className="provider-detail-empty">
                <Server aria-hidden="true" />
                <strong>没有可用的对话模型供应商</strong>
                <p>供应商目录加载完成后，可在这里配置连接和模型。</p>
              </div>
            </div>
          )}
        </section>
      </section>

      {editor && (
        <section
          className="settings-submodal-shell"
          ref={editorModalRef}
          tabIndex={-1}
          role="dialog"
          aria-modal="true"
          aria-label={editor.mode === 'create' ? '新增模型' : '编辑模型'}
        >
          <div className="model-editor-modal">
            <header>
              <span>
                <small>{editor.mode === 'create' ? '添加目录项' : '编辑目录项'}</small>
                <strong>{editor.mode === 'create' ? '新增模型' : editor.draft.name}</strong>
              </span>
              <button type="button" aria-label="关闭" onClick={() => setEditor(null)}>
                <X aria-hidden="true" />
              </button>
            </header>
            <div className="model-editor-form">
              <label>
                提供商
                <select
                  value={editor.draft.provider_id}
                  disabled={editor.mode === 'edit'}
                  onChange={(event) => updateEditor('provider_id', event.target.value)}
                >
                  {providers.map((provider) => (
                    <option value={provider.id} key={provider.id}>{provider.name}</option>
                  ))}
                </select>
                <small>模型必须属于现有提供商，API Key 不保存在模型上。</small>
              </label>
              <label>
                模型名称
                <input
                  autoFocus
                  value={editor.draft.name}
                  maxLength={128}
                  placeholder="例如 GLM 5.2"
                  onChange={(event) => updateEditor('name', event.target.value)}
                />
              </label>
              <label className="wide">
                模型 ID
                <input
                  value={editor.draft.model}
                  maxLength={256}
                  disabled={editor.mode === 'edit'}
                  placeholder="例如 glm-5.2"
                  onChange={(event) => updateEditor('model', event.target.value)}
                />
                <small>创建后不可修改，避免历史会话与运行快照失去身份。</small>
              </label>
              <label>
                上下文窗口
                <input
                  type="number"
                  min={1}
                  max={10_000_000}
                  value={editor.draft.context_window}
                  onChange={(event) => updateEditor('context_window', Number(event.target.value))}
                />
              </label>
              <label>
                默认最大输出
                <input
                  type="number"
                  min={1}
                  max={editor.draft.context_window}
                  value={editor.draft.default_max_output_tokens}
                  onChange={(event) =>
                    updateEditor('default_max_output_tokens', Number(event.target.value))
                  }
                />
              </label>
              <label className="wide">
                说明
                <textarea
                  value={editor.draft.notes}
                  maxLength={1000}
                  placeholder="可选：记录套餐、用途或能力边界"
                  onChange={(event) => updateEditor('notes', event.target.value)}
                />
              </label>
              <fieldset className="model-capability-checks wide">
                <legend>能力声明</legend>
                <label>
                  <input
                    type="checkbox"
                    checked={editor.draft.tags.includes('reasoning')}
                    onChange={(event) => toggleEditorTag('reasoning', event.target.checked)}
                  />
                  推理
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={editor.draft.tags.includes('tool')}
                    onChange={(event) => toggleEditorTag('tool', event.target.checked)}
                  />
                  工具调用
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={editor.draft.supports_usage}
                    onChange={(event) => updateEditor('supports_usage', event.target.checked)}
                  />
                  Usage
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={editor.draft.supports_reasoning_tokens}
                    onChange={(event) =>
                      updateEditor('supports_reasoning_tokens', event.target.checked)
                    }
                  />
                  推理 Token
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={editor.draft.supports_cached_tokens}
                    onChange={(event) =>
                      updateEditor('supports_cached_tokens', event.target.checked)
                    }
                  />
                  缓存 Token
                </label>
              </fieldset>
            </div>
            <footer>
              <button type="button" onClick={() => setEditor(null)}>取消</button>
              <button
                type="button"
                className="primary"
                disabled={busy || !editor.draft.provider_id || !editor.draft.model.trim() || !editor.draft.name.trim()}
                onClick={() => void submitEditor()}
              >
                {editor.mode === 'create' ? '创建模型' : '保存更改'}
              </button>
            </footer>
          </div>
        </section>
      )}

      {deleteTarget && (
        <section
          className="settings-submodal-shell"
          ref={deleteModalRef}
          tabIndex={-1}
          role="alertdialog"
          aria-modal="true"
          aria-label="删除模型"
        >
          <div className="model-delete-modal">
            <span className="danger-mark"><Trash2 aria-hidden="true" /></span>
            <h3>删除“{deleteTarget.name}”？</h3>
            <p>
              将从聊天选择器移除 {deleteTarget.model}。如果它仍是当前活动模型，系统会拒绝删除并保留数据。
            </p>
            <div>
              <button type="button" onClick={() => setDeleteTarget(null)}>取消</button>
              <button type="button" className="danger" disabled={busy} onClick={() => void confirmDeleteModel()}>
                删除模型
              </button>
            </div>
          </div>
        </section>
      )}
    </>
  );
}
