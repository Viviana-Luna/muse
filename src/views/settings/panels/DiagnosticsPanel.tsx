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
  const logs = [
    runtimeStatus ? `[RUNTIME] ${runtimeStatus}` : '[RUNTIME] 暂无运行时状态变更',
    voiceStatus ? `[VOICE] ${voiceStatus}` : '[VOICE] 语音状态未上报',
    modelFetchStatus.chat ? `[MODEL] ${modelFetchStatus.chat}` : '[MODEL] 模型列表尚未刷新',
    `[WORKSPACE] permission=${workspacePermissionMode}, sandbox=${workspaceSandboxMode}`
  ].slice(0, 50);

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
            <h2>只读日志视图</h2>
            <p>汇总最近的运行时、语音、模型和工具状态。</p>
          </div>
        </header>
        <div className="diagnostics-log" role="log" aria-label="最近运行状态">
          {logs.map((line) => (
            <code key={line}>{line}</code>
          ))}
        </div>
      </section>
    </div>
  );
}
