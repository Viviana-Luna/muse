import type { SettingsDialogProps } from '../types';

type WorkspacePanelProps = Pick<
  SettingsDialogProps,
  'busy' | 'workspacePermissionMode' | 'workspaceSandboxMode' | 'onUpdateWorkspacePolicy'
>;

export function WorkspacePanel({
  busy,
  workspacePermissionMode,
  workspaceSandboxMode,
  onUpdateWorkspacePolicy
}: WorkspacePanelProps) {
  return (
    <div className="settings-panel-body">
      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>新会话默认审批</h2>
            <p>这里只设置新会话默认值；当前会话请在输入框底栏切换。</p>
          </div>
        </header>
        <div className="permission-mode-list wide">
          <button
            type="button"
            className={workspacePermissionMode === 'request_approval' ? 'active info' : 'info'}
            onClick={() => void onUpdateWorkspacePolicy('request_approval', 'workspace_write')}
            disabled={busy}
          >
            <strong>手动审批</strong>
            <small>需要审批的动作由用户确认，权限限制在当前工作区。</small>
          </button>
          <button
            type="button"
            className={workspacePermissionMode === 'approve_for_me' ? 'active warning' : 'warning'}
            onClick={() => void onUpdateWorkspacePolicy('approve_for_me', 'workspace_write')}
            disabled={busy}
          >
            <strong>AUTO 模式</strong>
            <small>需要审批的动作交给独立审查器；异常时安全转人工。</small>
          </button>
        </div>
      </section>

      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>沙箱边界</h2>
            <p>查看当前文件访问范围和一次工具请求的审批路径。</p>
          </div>
        </header>
        <div className="sandbox-mode-card wide">
          <strong>当前沙箱</strong>
          <small>
            {workspaceSandboxMode === 'danger_full_access'
              ? '检测到旧版完全访问默认值；它不会在应用重启后恢复，请改为手动或 AUTO。'
              : '工作区写入：默认限制在当前项目工作区；YOLO 只能在当前会话输入区临时开启。'}
          </small>
        </div>
        <div className="workspace-flow wide" aria-label="工具审批流程">
          <span>AI 发起读取</span>
          <i />
          <span>检测允许目录</span>
          <i />
          <span>触发审批</span>
          <i />
          <span>允许通行</span>
        </div>
        <div className="workspace-root-summary wide">
          <strong>Codex 式工具边界</strong>
          <small>
            用户明确给出的外部路径会进入审批流程；模型需要先定位目标，再由 harness 根据权限模式和沙箱模式决定执行、审批或拒绝。
          </small>
        </div>
      </section>
    </div>
  );
}
