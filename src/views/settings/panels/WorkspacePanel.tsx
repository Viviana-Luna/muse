import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';

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
            <h2>审批策略</h2>
            <p>决定工具请求何时需要确认，以及允许自动执行的风险范围。</p>
          </div>
        </header>
        <div className="permission-mode-list wide">
          <button
            type="button"
            className={workspacePermissionMode === 'request_approval' ? 'active info' : 'info'}
            onClick={() => void onUpdateWorkspacePolicy('request_approval', 'workspace_write')}
            disabled={busy}
          >
            <strong>请求批准</strong>
            <small>写文件、联网和命令执行始终请求确认。</small>
          </button>
          <button
            type="button"
            className={workspacePermissionMode === 'approve_for_me' ? 'active warning' : 'warning'}
            onClick={() => void onUpdateWorkspacePolicy('approve_for_me', 'workspace_write')}
            disabled={busy}
          >
            <strong>替我审批</strong>
            <small>低风险动作自动执行；长期副作用仍按风险策略处理。</small>
          </button>
          <ConfirmDialog
            title="开启完全访问权限？"
            description="这将允许大模型访问和操作本地电脑上的所有文件与命令，有潜在数据丢失风险。"
            confirmLabel="开启完全访问"
            cancelLabel="继续保持限制"
            tone="danger"
            onConfirm={() => void onUpdateWorkspacePolicy('full_access', 'danger_full_access')}
          >
            <button
              type="button"
              className={workspacePermissionMode === 'full_access' ? 'active danger' : 'danger'}
              disabled={busy}
            >
              <strong>完全访问权限</strong>
              <small>不再请求审批，并允许访问本机任意路径。</small>
            </button>
          </ConfirmDialog>
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
              ? '全盘访问：文件工具不再限制目录。'
              : '工作区写入：默认限制在当前项目工作区；外部路径由本次操作审批决定。'}
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
