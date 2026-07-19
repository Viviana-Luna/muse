import type { ComponentProps } from 'react';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { DiagnosticsPanel } from './DiagnosticsPanel';

afterEach(cleanup);

function renderPanel(overrides: Partial<ComponentProps<typeof DiagnosticsPanel>> = {}) {
  const onRefreshDiagnostics = vi.fn();
  render(
    <DiagnosticsPanel
      settingsConfig={
        {
          chat: { api_base: 'https://api.example.com/v1' },
          tts: { api_base: '' },
          speech_recognition: { api_base: '', enabled: false }
        } as ComponentProps<typeof DiagnosticsPanel>['settingsConfig']
      }
      busy={false}
      modelFetchStatus={
        { chat: '已载入 8 个模型。' } as ComponentProps<
          typeof DiagnosticsPanel
        >['modelFetchStatus']
      }
      workspacePermissionMode="request_approval"
      workspaceSandboxMode="workspace_write"
      runtimeStatus="当前会话已更新。"
      voiceStatus="语音服务待命。"
      diagnosticsChecks={[]}
      onRefreshDiagnostics={onRefreshDiagnostics}
      {...overrides}
    />
  );
  return { onRefreshDiagnostics };
}

function statusRow(label: string) {
  const row = screen.getByText(label).closest('li');
  if (!row) throw new Error(`找不到系统状态行：${label}`);
  return within(row);
}

describe('DiagnosticsPanel', () => {
  it('把运行事实展示为当前系统状态，不再伪装成历史日志', () => {
    const { onRefreshDiagnostics } = renderPanel();

    expect(screen.getByRole('heading', { name: '当前系统状态' })).toBeVisible();
    expect(screen.getByRole('list', { name: '当前系统状态' })).toBeVisible();
    expect(screen.queryByRole('log')).not.toBeInTheDocument();
    expect(screen.queryByText('只读日志视图')).not.toBeInTheDocument();
    expect(statusRow('对话运行时').getByText('当前会话已更新。')).toBeVisible();
    expect(statusRow('语音服务').getByText('语音服务待命。')).toBeVisible();
    expect(statusRow('模型目录').getByText('已载入 8 个模型。')).toBeVisible();
    expect(
      statusRow('工作区权限').getByText('新会话审批：手动审批；文件边界：工作区写入。')
    ).toBeVisible();

    fireEvent.click(screen.getByRole('button', { name: '刷新检测' }));
    expect(onRefreshDiagnostics).toHaveBeenCalledTimes(1);
  });

  it('明确展示未上报状态和需要检查的旧版工作区策略', () => {
    renderPanel({
      runtimeStatus: '',
      voiceStatus: '',
      modelFetchStatus: {} as ComponentProps<typeof DiagnosticsPanel>['modelFetchStatus'],
      workspacePermissionMode: 'full_access',
      workspaceSandboxMode: 'danger_full_access'
    });

    expect(statusRow('对话运行时').getByText('尚未收到运行时状态。')).toBeVisible();
    expect(statusRow('语音服务').getByText('尚未收到语音服务状态。')).toBeVisible();
    expect(statusRow('模型目录').getByText('模型目录尚未刷新。')).toBeVisible();
    expect(
      statusRow('工作区权限').getByText(
        '新会话审批：旧版完全访问；文件边界：旧版完全文件访问。'
      )
    ).toBeVisible();
    expect(statusRow('工作区权限').getByText('需要检查')).toBeVisible();
  });
});
