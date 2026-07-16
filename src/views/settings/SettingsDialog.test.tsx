import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { SettingsDialog } from './SettingsDialog';
import type { SettingsDialogProps } from './types';

vi.mock('./panels/ChatPanel', () => ({ ChatPanel: () => <div>模型配置</div> }));
vi.mock('./panels/DiagnosticsPanel', () => ({ DiagnosticsPanel: () => <div>系统诊断</div> }));
vi.mock('./panels/AppearancePanel', () => ({ AppearancePanel: () => <div>外观配置</div> }));
vi.mock('./panels/SpeechRecognitionPanel', () => ({ SpeechRecognitionPanel: () => <div>语音识别</div> }));
vi.mock('./panels/TtsPanel', () => ({ TtsPanel: () => <div>语音配置</div> }));
vi.mock('./panels/WebSearchPanel', () => ({ WebSearchPanel: () => <div>联网搜索</div> }));
vi.mock('./panels/WorkspacePanel', () => ({ WorkspacePanel: () => <div>工具工作区</div> }));

afterEach(cleanup);

function createProps(settingsPanel: SettingsDialogProps['settingsPanel']): SettingsDialogProps {
  return {
    embedded: true,
    settingsPanel,
    busy: false,
    workspacePermissionMode: 'request_approval',
    dirtyDomains: [],
    setSettingsPanel: vi.fn(),
    onClose: vi.fn(),
    onDiscard: vi.fn(),
    onSubmit: vi.fn()
  } as unknown as SettingsDialogProps;
}

describe('SettingsDialog', () => {
  it('仅模型配置使用主从工作区，其余面板都直接归属右侧主框', () => {
    const { container, rerender } = render(<SettingsDialog {...createProps('chat')} />);

    expect(container.querySelector('.settings-panel')).toHaveClass('settings-panel-flush');
    expect(container.querySelector('.settings-panel')).not.toHaveClass('settings-panel-direct');

    const directPanels: SettingsDialogProps['settingsPanel'][] = [
      'tts',
      'speech_recognition',
      'appearance',
      'workspace',
      'web_search',
      'diagnostics'
    ];

    directPanels.forEach((settingsPanel) => {
      rerender(<SettingsDialog {...createProps(settingsPanel)} />);
      expect(container.querySelector('.settings-panel')).not.toHaveClass('settings-panel-flush');
      expect(container.querySelector('.settings-panel')).toHaveClass('settings-panel-direct');
      expect(container.querySelector('.settings-panel-scroll')).toBeInTheDocument();
    });

  });
});
