import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Cable,
  ArrowLeft,
  CircleCheck,
  CircleX,
  FlaskConical,
  Plus,
  RefreshCw,
  Save,
  Search,
  Trash2
} from 'lucide-react';
import {
  createMcpServer,
  deleteMcpServer,
  getMcpServer,
  listMcpServers,
  refreshMcpServer,
  testMcpServer,
  testMcpServerDraft,
  updateMcpServer
} from '@/api';
import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import { SectionLoading } from '@/components/feedback/LoadingState';
import type { AppToastInput } from '@/hooks/useAppToast';
import type {
  McpCatalogResponse,
  McpHttpTransportUpdate,
  McpSecretFieldState,
  McpSecretFieldUpdate,
  McpServerDetail,
  McpServerDraft,
  McpServerSummary,
  McpStdioTransportUpdate
} from '@/types';

const MCP_SERVER_NAME_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;
const MCP_SERVER_NAME_HINT = '仅允许 1-64 个英文字母、数字、下划线或短横线。';

function emptyDraft(type: 'stdio' | 'streamable_http' = 'stdio'): McpServerDraft {
  return {
    name: '',
    enabled: true,
    request_timeout_ms: 30_000,
    enabled_tools: null,
    disabled_tools: [],
    approval_policy: 'always_ask',
    tool_approval_overrides: {},
    transport:
      type === 'stdio'
        ? { type: 'stdio', command: '', args: [], cwd: '', env: {}, secrets: [] }
        : { type: 'streamable_http', url: '', headers: {}, header_secrets: [], bearer_token: null }
  };
}

function secretStateToUpdate(secret: McpSecretFieldState): McpSecretFieldUpdate {
  return {
    target: secret.target,
    secret: { action: 'keep' }
  };
}

function detailToDraft(detail: McpServerDetail): McpServerDraft {
  const transport = detail.transport.type === 'stdio'
    ? {
        type: 'stdio' as const,
        command: detail.transport.command,
        args: detail.transport.args,
        cwd: detail.transport.cwd,
        env: detail.transport.env,
        secrets: detail.transport.secrets.map(secretStateToUpdate)
      }
    : {
        type: 'streamable_http' as const,
        url: detail.transport.url,
        headers: detail.transport.headers,
        header_secrets: detail.transport.header_secrets.map(secretStateToUpdate),
        bearer_token: detail.transport.bearer_token
          ? secretStateToUpdate(detail.transport.bearer_token)
          : null
      };
  return {
    name: detail.name,
    enabled: detail.enabled,
    request_timeout_ms: detail.request_timeout_ms ?? 30_000,
    enabled_tools: detail.enabled_tools,
    disabled_tools: detail.disabled_tools,
    approval_policy: detail.approval_policy,
    tool_approval_overrides: detail.tool_approval_overrides,
    transport
  };
}

function pairsToText(values: Record<string, string>) {
  return Object.entries(values).map(([key, value]) => `${key}=${value}`).join('\n');
}

function textToPairs(value: string) {
  return Object.fromEntries(
    value
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const index = line.indexOf('=');
        return index < 0 ? [line, ''] : [line.slice(0, index).trim(), line.slice(index + 1).trim()];
      })
      .filter(([key]) => Boolean(key))
  );
}

interface McpPageProps {
  selectedName?: string;
  onSelectedNameChange: (name?: string) => void;
  notify: (input: AppToastInput) => void;
}

export function McpPage({ selectedName, onSelectedNameChange, notify }: McpPageProps) {
  const [servers, setServers] = useState<McpServerSummary[]>([]);
  const [record, setRecord] = useState<McpServerDetail | null>(null);
  const [draft, setDraft] = useState<McpServerDraft>(() => emptyDraft());
  const [query, setQuery] = useState('');
  const [creating, setCreating] = useState(false);
  const [listLoading, setListLoading] = useState(true);
  const [recordLoading, setRecordLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [nameError, setNameError] = useState('');
  const [catalog, setCatalog] = useState<McpCatalogResponse | null>(null);
  const [catalogTab, setCatalogTab] = useState<'tools' | 'resources'>('tools');
  const dirty = useMemo(
    () => creating || (record ? JSON.stringify(draft) !== JSON.stringify(detailToDraft(record)) : false),
    [creating, draft, record]
  );
  const filteredServers = servers.filter((server) =>
    server.name.toLowerCase().includes(query.trim().toLowerCase())
  );
  const showRecordLoading = Boolean(selectedName && !creating && recordLoading);

  const reloadList = useCallback(async () => {
    const next = await listMcpServers();
    setServers(next);
    return next;
  }, []);

  useEffect(() => {
    let cancelled = false;
    setListLoading(true);
    void reloadList()
      .then((items) => {
        if (!cancelled && !selectedName && items.length > 0) onSelectedNameChange(items[0].name);
      })
      .catch((error: Error) =>
        notify({ title: '无法读取 MCP 连接', description: error.message, tone: 'error' })
      )
      .finally(() => {
        if (!cancelled) setListLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [reloadList]);

  useEffect(() => {
    if (!selectedName || creating) return;
    setRecordLoading(true);
    setCatalog(null);
    void getMcpServer(selectedName)
      .then((next) => {
        setRecord(next);
        setDraft(detailToDraft(next));
      })
      .catch((error: Error) =>
        notify({ title: '无法读取 MCP 连接', description: error.message, tone: 'error' })
      )
      .finally(() => setRecordLoading(false));
  }, [selectedName, creating]);

  function requestSelection(name: string) {
    if (dirty && !window.confirm('当前 MCP 连接有未保存更改，确定放弃并切换吗？')) return;
    setCreating(false);
    setRecordLoading(true);
    setCatalog(null);
    setNameError('');
    onSelectedNameChange(name);
  }

  function beginCreate() {
    if (dirty && !window.confirm('当前 MCP 连接有未保存更改，确定放弃并新建吗？')) return;
    setCreating(true);
    setRecordLoading(false);
    setRecord(null);
    setDraft(emptyDraft());
    setCatalog(null);
    setNameError('');
    onSelectedNameChange(undefined);
  }

  async function save() {
    const normalizedName = draft.name.trim();
    if (!MCP_SERVER_NAME_PATTERN.test(normalizedName)) {
      setNameError(MCP_SERVER_NAME_HINT);
      notify({
        title: 'MCP 名称不符合要求',
        description: MCP_SERVER_NAME_HINT,
        tone: 'warning'
      });
      return;
    }

    const normalizedDraft = { ...draft, name: normalizedName };
    setNameError('');
    setSaving(true);
    try {
      const saved = creating
        ? await createMcpServer(normalizedDraft)
        : await updateMcpServer(record!.name, record!.revision, normalizedDraft);
      setCreating(false);
      setRecord(saved);
      setDraft(detailToDraft(saved));
      await reloadList();
      onSelectedNameChange(saved.name);
      notify({ title: 'MCP 连接已保存', description: saved.name, tone: 'success' });
    } catch (error) {
      notify({ title: 'MCP 保存失败', description: (error as Error).message, tone: 'error' });
    } finally {
      setSaving(false);
    }
  }

  async function toggleEnabled() {
    if (!record) {
      setDraft({ ...draft, enabled: !draft.enabled });
      return;
    }
    if (dirty && !window.confirm('启停操作会放弃当前未保存更改，确定继续吗？')) return;
    setSaving(true);
    try {
      const baseline = detailToDraft(record);
      const saved = await updateMcpServer(record.name, record.revision, {
        ...baseline,
        enabled: !record.enabled
      });
      setRecord(saved);
      setDraft(detailToDraft(saved));
      await reloadList();
      notify({ title: saved.enabled ? 'MCP 已启用' : 'MCP 已停用', description: saved.name, tone: 'success' });
    } catch (error) {
      notify({ title: '启停失败', description: (error as Error).message, tone: 'error' });
    } finally {
      setSaving(false);
    }
  }

  async function runTest(refresh: boolean) {
    const normalizedName = draft.name.trim();
    if (!MCP_SERVER_NAME_PATTERN.test(normalizedName)) {
      setNameError(MCP_SERVER_NAME_HINT);
      notify({ title: 'MCP 名称不符合要求', description: MCP_SERVER_NAME_HINT, tone: 'warning' });
      return;
    }
    setTesting(true);
    try {
      const testDraft = { ...draft, name: normalizedName };
      const result = !creating && record && !dirty
        ? refresh
          ? await refreshMcpServer(record.name)
          : await testMcpServer(record.name)
        : await testMcpServerDraft(testDraft, {
            sourceName: record?.name,
            sourceRevision: record?.revision,
            includeResources: refresh
          });
      setCatalog(result);
      if (!creating && record && !dirty) await reloadList();
      notify({
        title: result.status === 'connected' ? 'MCP 连接成功' : 'MCP 连接异常',
        description: result.errors[0]
          ? String(result.errors[0].message ?? '请查看结构化诊断')
          : `${result.tools.length} 个工具`,
        tone: result.status === 'connected' ? 'success' : 'error'
      });
    } catch (error) {
      notify({ title: 'MCP 测试失败', description: (error as Error).message, tone: 'error' });
    } finally {
      setTesting(false);
    }
  }

  async function remove() {
    if (!record) return;
    try {
      await deleteMcpServer(record.name, record.revision);
      const next = await reloadList();
      setRecord(null);
      setCatalog(null);
      onSelectedNameChange(next[0]?.name);
      notify({ title: 'MCP 连接已删除', description: record.name, tone: 'success' });
    } catch (error) {
      notify({ title: 'MCP 删除失败', description: (error as Error).message, tone: 'error' });
    }
  }

  function showList() {
    if (dirty && !window.confirm('当前 MCP 连接有未保存更改，确定放弃并返回列表吗？')) return;
    setCreating(false);
    setRecordLoading(false);
    setRecord(null);
    setCatalog(null);
    setNameError('');
    onSelectedNameChange(undefined);
  }

  function updateStdio(patch: Partial<McpStdioTransportUpdate>) {
    if (draft.transport.type !== 'stdio') return;
    setDraft({ ...draft, transport: { ...draft.transport, ...patch } });
  }

  function updateHttp(patch: Partial<McpHttpTransportUpdate>) {
    if (draft.transport.type !== 'streamable_http') return;
    setDraft({ ...draft, transport: { ...draft.transport, ...patch } });
  }

  function updateToolApproval(toolName: string, policy: '' | 'always_ask' | 'trusted_read_only') {
    const overrides = { ...draft.tool_approval_overrides };
    if (policy) overrides[toolName] = policy;
    else delete overrides[toolName];
    setDraft({ ...draft, tool_approval_overrides: overrides });
  }

  const catalogItems = catalogTab === 'tools' ? catalog?.tools ?? [] : catalog?.resources ?? [];

  return (
    <section className={`management-page mcp-page ${creating || record || selectedName ? 'show-detail' : 'show-list'}`} aria-label="MCP 管理">
      <aside className="object-list-pane">
        <header>
          <span><small>手动添加</small><strong>MCP 服务器</strong></span>
          <button type="button" className="accent-outline" onClick={beginCreate}><Plus aria-hidden="true" />添加 MCP</button>
        </header>
        <label className="management-search"><Search aria-hidden="true" /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索连接" /></label>
        <div className="object-list" role="list">
          {listLoading && servers.length === 0 && (
            <SectionLoading label="正在读取 MCP 连接" variant="surface" />
          )}
          {filteredServers.map((server) => (
            <button type="button" key={server.name} role="listitem" className={!creating && selectedName === server.name ? 'active' : ''} onClick={() => requestSelection(server.name)}>
              <Cable aria-hidden="true" />
              <span>
                <strong>{server.name}</strong>
                <small>{server.transport === 'stdio' ? '本地命令 · stdio' : '远程连接 · HTTP'}</small>
                <small>{server.status === 'connected'
                  ? `${server.tool_count} 工具 · ${server.resource_count} 资源`
                  : server.status === 'failed'
                    ? server.last_error?.message ?? '最近检查失败'
                    : server.status === 'policy_blocked'
                      ? '已停用，策略未连接'
                      : '当前 revision 尚未测试'}</small>
              </span>
              <i className={server.status === 'connected' ? 'enabled' : ''}>{server.status === 'connected' ? '正常' : server.enabled ? '待检查' : '停用'}</i>
            </button>
          ))}
          {!listLoading && filteredServers.length === 0 && (
            <div className="object-list-empty"><Cable aria-hidden="true" /><strong>{servers.length === 0 ? '还没有 MCP 连接' : '没有匹配结果'}</strong><span>{servers.length === 0 ? '点击“添加 MCP”，手动填写一个连接。' : '换个关键词试试。'}</span></div>
          )}
        </div>
      </aside>

      <section className="object-detail-pane">
        {showRecordLoading ? (
          <SectionLoading
            label="正在加载 MCP 连接"
            description={selectedName}
          />
        ) : creating || record ? (
          <>
            <header className="detail-toolbar mcp-detail-toolbar">
              <span><button type="button" className="mobile-list-back" aria-label="返回 MCP 列表" onClick={showList}><ArrowLeft aria-hidden="true" /></button><Cable aria-hidden="true" /><strong>{creating ? '添加 MCP' : record?.name}</strong>{catalog?.status === 'connected' && <small className="success"><CircleCheck aria-hidden="true" />已连接</small>}{catalog?.status === 'failed' && <small className="danger"><CircleX aria-hidden="true" />异常</small>}</span>
              <button type="button" className={`switch ${draft.enabled ? 'on' : ''}`} disabled={saving} aria-pressed={draft.enabled} aria-label={draft.enabled ? '停用连接' : '启用连接'} onClick={() => void toggleEnabled()}><i /></button>
            </header>
            <div className="mcp-detail-body">
              <section className="mcp-form-section">
                <h2>基础配置</h2>
                <div className="mcp-form-grid">
                  <label>
                    <span>名称</span>
                    <input
                      name="mcp_name"
                      aria-label="名称"
                      value={draft.name}
                      maxLength={64}
                      pattern={'[A-Za-z0-9_\\-]{1,64}'}
                      aria-invalid={Boolean(nameError)}
                      aria-describedby="mcp-name-hint"
                      onChange={(event) => {
                        setDraft({ ...draft, name: event.target.value });
                        if (nameError) setNameError('');
                      }}
                    />
                    <small
                      id="mcp-name-hint"
                      className={nameError ? 'field-error' : 'field-hint'}
                      role={nameError ? 'alert' : undefined}
                    >
                      {nameError || MCP_SERVER_NAME_HINT}
                    </small>
                  </label>
                  <label><span>传输方式</span><select disabled={!creating} value={draft.transport.type} onChange={(event) => { const next = emptyDraft(event.target.value as 'stdio' | 'streamable_http'); setDraft({ ...next, name: draft.name, enabled: draft.enabled, request_timeout_ms: draft.request_timeout_ms, enabled_tools: draft.enabled_tools, disabled_tools: draft.disabled_tools, approval_policy: draft.approval_policy, tool_approval_overrides: draft.tool_approval_overrides }); }}><option value="stdio">stdio</option><option value="streamable_http">streamable_http</option></select></label>
                </div>
              </section>

              {draft.transport.type === 'stdio' ? (
                <section className="mcp-form-section">
                  <h2>命令配置</h2>
                  <div className="mcp-form-grid">
                    <label><span>命令</span><input value={draft.transport.command} placeholder="例如 npx" onChange={(event) => updateStdio({ command: event.target.value })} /></label>
                    <label><span>参数（每行一个）</span><textarea rows={3} value={draft.transport.args.join('\n')} onChange={(event) => updateStdio({ args: event.target.value.split('\n').map((item) => item.trim()).filter(Boolean) })} /></label>
                    <label className="wide"><span>工作目录（可选）</span><input value={draft.transport.cwd ?? ''} onChange={(event) => updateStdio({ cwd: event.target.value })} /></label>
                    <label className="wide"><span>普通环境变量（每行 KEY=VALUE）</span><textarea rows={3} value={pairsToText(draft.transport.env)} onChange={(event) => updateStdio({ env: textToPairs(event.target.value) })} /></label>
                  </div>
                  <SecretRows
                    label="秘密环境变量"
                    values={draft.transport.secrets}
                    onChange={(secrets) => updateStdio({ secrets })}
                    defaultTarget="API_TOKEN"
                  />
                </section>
              ) : (
                <section className="mcp-form-section">
                  <h2>HTTP 配置</h2>
                  <div className="mcp-form-grid">
                    <label className="wide"><span>URL</span><input type="url" value={draft.transport.url} placeholder="https://example.com/mcp" onChange={(event) => updateHttp({ url: event.target.value })} /></label>
                    <label className="wide"><span>普通 Header（每行 KEY=VALUE）</span><textarea rows={3} value={pairsToText(draft.transport.headers)} onChange={(event) => updateHttp({ headers: textToPairs(event.target.value) })} /></label>
                  </div>
                  <SecretRows label="秘密 Header" values={draft.transport.header_secrets} onChange={(header_secrets) => updateHttp({ header_secrets })} defaultTarget="X-API-Key" />
                  <SecretRows label="Bearer 凭据" values={draft.transport.bearer_token ? [draft.transport.bearer_token] : []} onChange={(values) => updateHttp({ bearer_token: values[0] ?? null })} defaultTarget="Authorization" limit={1} />
                </section>
              )}

              <section className="mcp-form-section mcp-trust-section">
                <h2>本地授权</h2>
                <div className="mcp-form-grid">
                  <label>
                    <span>默认审批策略</span>
                    <select
                      value={draft.approval_policy}
                      onChange={(event) => setDraft({
                        ...draft,
                        approval_policy: event.target.value as McpServerDraft['approval_policy']
                      })}
                    >
                      <option value="always_ask">每次询问</option>
                      <option value="trusted_read_only">信任远端只读声明</option>
                    </select>
                  </label>
                  <div className="mcp-trust-explanation">
                    <strong>远端声明不等于本地授权</strong>
                    <small>只有你明确启用信任，且工具声明 readOnlyHint=true 时才免审批；写操作始终询问。</small>
                    <button
                      type="button"
                      onClick={() => setDraft({
                        ...draft,
                        approval_policy: 'always_ask',
                        tool_approval_overrides: Object.fromEntries(
                          Object.entries(draft.tool_approval_overrides)
                            .filter(([, policy]) => policy === 'always_ask')
                        )
                      })}
                    >
                      撤销全部只读信任
                    </button>
                  </div>
                </div>
              </section>

              <details className="mcp-advanced">
                <summary>高级设置</summary>
                <div className="mcp-form-grid">
                  <label><span>请求超时（毫秒）</span><input type="number" min={1000} max={120000} value={draft.request_timeout_ms ?? 30000} onChange={(event) => setDraft({ ...draft, request_timeout_ms: Number(event.target.value) })} /></label>
                  <label><span>排除工具（每行一个）</span><textarea rows={3} value={draft.disabled_tools.join('\n')} onChange={(event) => setDraft({ ...draft, disabled_tools: event.target.value.split('\n').map((item) => item.trim()).filter(Boolean) })} /></label>
                  <label className="wide"><span>仅包含工具（留空表示全部）</span><textarea rows={3} value={(draft.enabled_tools ?? []).join('\n')} onChange={(event) => { const values = event.target.value.split('\n').map((item) => item.trim()).filter(Boolean); setDraft({ ...draft, enabled_tools: values.length ? values : null }); }} /></label>
                </div>
              </details>

              <section className="mcp-catalog-section">
                <header>
                  <div role="tablist" aria-label="MCP 目录">
                    <button type="button" role="tab" aria-selected={catalogTab === 'tools'} className={catalogTab === 'tools' ? 'active' : ''} onClick={() => setCatalogTab('tools')}>工具 {catalog ? catalog.tools.length : ''}</button>
                    <button type="button" role="tab" aria-selected={catalogTab === 'resources'} className={catalogTab === 'resources' ? 'active' : ''} onClick={() => setCatalogTab('resources')}>资源 {catalog ? catalog.resources.length : ''}</button>
                  </div>
                  <button type="button" disabled={testing} onClick={() => void runTest(true)}><RefreshCw aria-hidden="true" />刷新目录</button>
                </header>
                {catalog ? (
                  <>
                    <div className="mcp-diagnostic-summary">
                      <span><strong>状态</strong>{catalog.status === 'connected' ? '连接成功' : '连接异常'}</span>
                      <span><strong>协议</strong>{catalog.diagnostic.connection?.protocol_version ?? '未协商'}</span>
                      <span><strong>初始化</strong>{catalog.diagnostic.connection?.initialized_at ?? '未完成'}</span>
                      <span><strong>网络</strong>禁止重定向 · 不使用系统代理</span>
                    </div>
                    {catalog.errors[0] && (
                      <div className="mcp-diagnostic-error" role="alert">
                        <strong>{String(catalog.errors[0].code ?? 'mcp_connection_failed')}</strong>
                        <span>{String(catalog.errors[0].message ?? '连接失败')}</span>
                      </div>
                    )}
                    {(catalog.diagnostic.connection?.stderr_summary || catalog.diagnostic.connection?.eviction_reason) && (
                      <details className="mcp-local-diagnostic">
                        <summary>本地诊断详情</summary>
                        {catalog.diagnostic.connection.stderr_summary && (
                          <pre>{catalog.diagnostic.connection.stderr_summary}</pre>
                        )}
                        {catalog.diagnostic.connection.eviction_reason && (
                          <small>连接移除原因：{catalog.diagnostic.connection.eviction_reason}</small>
                        )}
                      </details>
                    )}
                    {catalogItems.length > 0 ? (
                      <div className="mcp-catalog-list">
                        {catalogItems.map((item, index) => {
                          const itemName = String(item.name ?? item.uri ?? index);
                          const local = (item.local_approval ?? {}) as Record<string, unknown>;
                          return (
                            <article key={itemName}>
                              <strong>{String(item.name ?? item.uri ?? '未命名')}</strong>
                              <small>{String(item.description ?? item.mime_type ?? '')}</small>
                              {catalogTab === 'tools' && (
                                <>
                                  <small>{item.read_only === true ? '远端声明：只读' : '远端声明：可能产生副作用'}</small>
                                  <small>本地判定：{String(local.final_risk ?? 'external_side_effect')} · {local.requires_approval === false ? '免审批' : '需要审批'}</small>
                                  <label>
                                    <span>单工具覆盖</span>
                                    <select
                                      value={draft.tool_approval_overrides[itemName] ?? ''}
                                      onChange={(event) => updateToolApproval(
                                        itemName,
                                        event.target.value as '' | 'always_ask' | 'trusted_read_only'
                                      )}
                                    >
                                      <option value="">继承 Server 策略</option>
                                      <option value="always_ask">始终询问</option>
                                      <option value="trusted_read_only">信任只读声明</option>
                                    </select>
                                  </label>
                                </>
                              )}
                            </article>
                          );
                        })}
                      </div>
                    ) : <p>{catalogTab === 'tools' ? '该连接没有返回工具。' : '该连接没有返回资源。'}</p>}
                  </>
                ) : <p>无需保存即可测试当前草稿；测试只会连接这个 Server，也不会写入配置。</p>}
              </section>
            </div>
            <footer className="detail-actions mcp-detail-actions">
              {!creating && record && <ConfirmDialog title="删除这个 MCP 连接？" description={`“${record.name}”及其凭据绑定将被永久删除。`} confirmLabel="删除" tone="danger" onConfirm={() => void remove()}><button type="button" className="danger-ghost"><Trash2 aria-hidden="true" />删除</button></ConfirmDialog>}
              <span />
              <button type="button" disabled={testing} onClick={() => void runTest(false)}><FlaskConical aria-hidden="true" />{testing ? '测试中…' : '测试连接'}</button>
              <button type="button" className="primary" disabled={saving || (!creating && !dirty)} onClick={() => void save()}><Save aria-hidden="true" />{saving ? '保存中…' : '保存更改'}</button>
            </footer>
          </>
        ) : (
          <div className="management-empty-detail"><Cable aria-hidden="true" /><h1>手动添加 MCP 连接</h1><p>选择 stdio 或 streamable_http，然后填写服务器实际需要的连接参数。</p><button type="button" className="primary" onClick={beginCreate}><Plus aria-hidden="true" />添加 MCP</button></div>
        )}
      </section>
    </section>
  );
}

function SecretRows({
  label,
  values,
  onChange,
  defaultTarget,
  limit
}: {
  label: string;
  values: McpSecretFieldUpdate[];
  onChange: (values: McpSecretFieldUpdate[]) => void;
  defaultTarget: string;
  limit?: number;
}) {
  function update(index: number, patch: Partial<McpSecretFieldUpdate>) {
    onChange(values.map((item, itemIndex) => itemIndex === index ? { ...item, ...patch } : item));
  }
  return (
    <section className="mcp-secret-fields">
      <header><strong>{label}</strong>{(!limit || values.length < limit) && <button type="button" onClick={() => onChange([...values, { target: defaultTarget, secret: { action: 'replace', value: '' } }])}><Plus aria-hidden="true" />添加</button>}</header>
      {values.map((field, index) => (
        <div className="mcp-secret-row" key={`${field.target}-${index}`}>
          <label><span>名称</span><input value={field.target} placeholder="例如 GITHUB_TOKEN" onChange={(event) => update(index, { target: event.target.value })} /></label>
          <label>
            <span>凭据值</span>
            <input
              type="password"
              autoComplete="off"
              value={field.secret.value ?? ''}
              placeholder={field.secret.action === 'keep' ? '已配置，留空保持不变' : '输入后安全保存'}
              onChange={(event) =>
                update(index, { secret: { action: 'replace', value: event.target.value } })
              }
            />
          </label>
          <button type="button" aria-label={`删除 ${field.target || label}`} onClick={() => { if (field.secret.action === 'keep') update(index, { secret: { action: 'delete' } }); else onChange(values.filter((_, itemIndex) => itemIndex !== index)); }}><Trash2 aria-hidden="true" /></button>
          {field.secret.action === 'delete' && <small>保存后删除</small>}
        </div>
      ))}
    </section>
  );
}
