import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { SkillPickerController } from '@/views/chat/hooks/useSkillPicker';
import { ComposerBar } from './ComposerBar';

function createSkillPicker(
  overrides: Partial<SkillPickerController> = {}
): SkillPickerController {
  return {
    open: false,
    skills: [],
    selectedSkill: null,
    loading: false,
    error: null,
    omittedSkillCount: 0,
    openPicker: vi.fn(),
    closePicker: vi.fn(),
    selectSkill: vi.fn(),
    clearSelection: vi.fn(),
    refresh: vi.fn(),
    ...overrides
  };
}

describe('ComposerBar', () => {
  afterEach(cleanup);

  it('始终展示输入框，不再要求先点击回复', () => {
    render(
      <ComposerBar
        busy={false}
        inputValue=""
        skillPicker={createSkillPicker()}
        voice={{
          status: '语音未启用',
          listening: false,
          speechRecognitionAvailable: false,
          ttsAvailable: false,
          voiceEnabled: false
        } as never}
        sendIcon={<span>发送图标</span>}
        onInputValueChange={vi.fn()}
        onSend={vi.fn()}
        onStartVoiceInput={vi.fn()}
        onToggleVoice={vi.fn()}
        onStopVoice={vi.fn()}
      />
    );

    expect(screen.getByPlaceholderText('对角色说点什么…')).toBeVisible();
    expect(screen.queryByRole('button', { name: '回复' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '语音输入' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '语音播报' })).not.toBeInTheDocument();
  });

  it('角色快照不可写时锁定写操作，但保留停止语音播报能力', () => {
    const onStopVoice = vi.fn();
    const view = render(
      <ComposerBar
        busy={false}
        inputValue="尚未发送的消息"
        skillPicker={createSkillPicker()}
        voice={{
          status: '语音已就绪',
          listening: false,
          speechRecognitionAvailable: true,
          ttsAvailable: true,
          voiceEnabled: true
        } as never}
        sendIcon={<span>发送图标</span>}
        sendDisabledReason="角色状态已过期，请先重试同步。"
        onInputValueChange={vi.fn()}
        onSend={vi.fn()}
        onStartVoiceInput={vi.fn()}
        onToggleVoice={vi.fn()}
        onStopVoice={onStopVoice}
      />
    );

    const composer = within(view.container);
    expect(composer.getByRole('textbox')).toHaveAttribute('readonly');
    expect(composer.getByRole('button', { name: '语音输入' })).toBeDisabled();
    expect(composer.getByRole('button', { name: '关闭播报' })).toBeDisabled();
    const stopVoice = composer.getByRole('button', { name: '停止语音播报' });
    expect(stopVoice).toBeEnabled();
    stopVoice.click();
    expect(onStopVoice).toHaveBeenCalledTimes(1);
    expect(composer.getByRole('button', { name: '发送' })).toBeDisabled();
    expect(composer.getByRole('button', { name: 'Skill' })).toBeDisabled();
  });

  it('展示已选择的 Skill 附件并允许在空闲时移除', () => {
    const openPicker = vi.fn();
    const clearSelection = vi.fn();
    render(
      <ComposerBar
        busy={false}
        inputValue="生成一份报告"
        skillPicker={createSkillPicker({
          selectedSkill: {
            name: 'pdf',
            description: '读取和生成 PDF',
            revision: 'revision-pdf',
            source: 'user_store'
          },
          openPicker,
          clearSelection
        })}
        voice={{
          status: '语音未启用',
          listening: false,
          speechRecognitionAvailable: false,
          ttsAvailable: false,
          voiceEnabled: false
        } as never}
        sendIcon={<span>发送图标</span>}
        onInputValueChange={vi.fn()}
        onSend={vi.fn()}
        onStartVoiceInput={vi.fn()}
        onToggleVoice={vi.fn()}
        onStopVoice={vi.fn()}
      />
    );

    expect(screen.getByLabelText('已选择 Skill：pdf')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '移除 Skill pdf' }));
    expect(clearSelection).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: 'Skill' }));
    expect(openPicker).toHaveBeenCalledTimes(1);
  });
});
