import type { SettingsDialogProps } from '../types';

type DiagnosticsPanelProps = Pick<
  SettingsDialogProps,
  | 'settingsConfig'
  | 'busy'
  | 'modelFetchStatus'
  | 'workspacePermissionMode'
  | 'workspaceSandboxMode'
  | 'runtimeStatus'
  | 'voiceStatus'
  | 'diagnosticsChecks'
  | 'onRefreshDiagnostics'
>;

type SystemStatusTone = 'info' | 'warning' | 'danger';

interface SystemStatusItem {
  id: string;
  label: string;
  detail: string;
  badge: string;
  tone: SystemStatusTone;
}

function permissionModeLabel(value: string) {
  switch (value) {
    case 'request_approval':
      return '手动审批';
    case 'approve_for_me':
      return 'AUTO 模式';
    case 'full_access':
      return '旧版完全访问';
    default:
      return `未知权限模式（${value || '空值'}）`;
  }
}

function sandboxModeLabel(value: string) {
  switch (value) {
    case 'workspace_write':
      return '工作区写入';
    case 'danger_full_access':
      return '旧版完全文件访问';
    default:
      return `未知沙箱模式（${value || '空值'}）`;
  }
}

export function DiagnosticsPanel({
  settingsConfig,
  busy,
  modelFetchStatus,
  workspacePermissionMode,
  workspaceSandboxMode,
  runtimeStatus,
  voiceStatus,
  diagnosticsChecks,
  onRefreshDiagnostics
}: DiagnosticsPanelProps) {
  const checks =
    diagnosticsChecks.length > 0
      ? diagnosticsChecks
      : [
          {
            id: 'chat',
            label: '对话模型 API',
            target: settingsConfig.chat.api_base || '未配置',
            status: settingsConfig.chat.api_base ? 'skipped' : 'skipped',
            latency_ms: null,
            message: settingsConfig.chat.api_base ? '等待手动刷新检测。' : '未配置 API Base。'
          },
          {
            id: 'tts',
            label: 'TTS API',
            target: settingsConfig.tts.api_base || '未配置',
            status: settingsConfig.tts.api_base ? 'skipped' : 'skipped',
            latency_ms: null,
            message: settingsConfig.tts.api_base ? '等待手动刷新检测。' : '未配置 API Base。'
          },
          {
            id: 'asr',
            label: '语音识别 API',
            target: settingsConfig.speech_recognition.api_base || '未配置',
            status: settingsConfig.speech_recognition.enabled ? 'skipped' : 'skipped',
            latency_ms: null,
            message: settingsConfig.speech_recognition.enabled ? '等待手动刷新检测。' : '未启用。'
          }
        ];
  const workspaceNeedsAttention =
    workspacePermissionMode === 'full_access' ||
    workspaceSandboxMode === 'danger_full_access' ||
    !['request_approval', 'approve_for_me'].includes(workspacePermissionMode) ||
    workspaceSandboxMode !== 'workspace_write';
  const systemStatuses: SystemStatusItem[] = [
    {
      id: 'runtime',
      label: '对话运行时',
      detail: runtimeStatus || '尚未收到运行时状态。',
      badge: runtimeStatus ? '当前状态' : '未上报',
      tone: runtimeStatus ? 'info' : 'warning'
    },
    {
      id: 'voice',
      label: '语音服务',
      detail: voiceStatus || '尚未收到语音服务状态。',
      badge: voiceStatus ? '当前状态' : '未上报',
      tone: voiceStatus ? 'info' : 'warning'
    },
    {
      id: 'model',
      label: '模型目录',
      detail: modelFetchStatus.chat || '模型目录尚未刷新。',
      badge: modelFetchStatus.chat ? '当前状态' : '未刷新',
      tone: modelFetchStatus.chat ? 'info' : 'warning'
    },
    {
      id: 'workspace',
      label: '工作区权限',
      detail: `新会话审批：${permissionModeLabel(workspacePermissionMode)}；文件边界：${sandboxModeLabel(workspaceSandboxMode)}。`,
      badge: workspaceNeedsAttention ? '需要检查' : '当前策略',
      tone: workspaceNeedsAttention ? 'danger' : 'info'
    }
  ];

  return (
    <div className="settings-panel-body diagnostics-grid">
      <section className="settings-module diagnostics-card">
        <header className="diagnostics-card-head">
          <div>
            <h2>连通性仪</h2>
            <p>检查模型与语音服务当前是否可达。</p>
          </div>
          <button type="button" onClick={() => void onRefreshDiagnostics()} disabled={busy}>
            刷新检测
          </button>
        </header>
        <div className="connectivity-list">
          {checks.map((check) => (
            <span key={check.id}>
              <strong>{check.label}</strong>
              <small title={check.message}>
                {check.target}
                {typeof check.latency_ms === 'number' ? ` / ${check.latency_ms}ms` : ''}
              </small>
              <i className={check.status === 'reachable' ? 'success' : check.status === 'failed' ? 'danger' : 'warning'}>
                {check.status === 'reachable'
                  ? '已响应'
                  : check.status === 'failed'
                    ? '失败'
                    : '待检测'}
              </i>
            </span>
          ))}
        </div>
      </section>
      <section className="settings-module diagnostics-card">
        <header className="settings-module-head">
          <div>
            <h2>当前系统状态</h2>
            <p>展示此刻的运行时、语音、模型目录和工作区策略，不保存历史记录。</p>
          </div>
        </header>
        <ul className="system-status-list" aria-label="当前系统状态">
          {systemStatuses.map((item) => (
            <li key={item.id}>
              <strong>{item.label}</strong>
              <small title={item.detail}>{item.detail}</small>
              <span className={`system-status-badge ${item.tone}`}>{item.badge}</span>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
