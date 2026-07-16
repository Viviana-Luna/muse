import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ModelsConfig } from '@/types';
import { TtsPanel } from './TtsPanel';

const settingsConfig = {
  tts: {
    enabled: true,
    provider: 'openai_audio_speech',
    api_base: 'https://api.example.com/v1',
    api_key: null,
    api_key_configured: true,
    model: 'qwen3-tts-base',
    voice_id: 'default',
    speed: 1,
    response_format: 'mp3'
  }
} as ModelsConfig;

function renderPanel(onPreviewTts: () => Promise<void>) {
  return render(
    <TtsPanel
      settingsConfig={settingsConfig}
      busy={false}
      maskedKeys={{ chat: '', tts: '••••••••', asr: '', audio_understanding: '' }}
      setSettingsConfig={vi.fn()}
      onPreviewTts={onPreviewTts}
    />
  );
}

afterEach(cleanup);

describe('TtsPanel', () => {
  it('试听结果交给统一通知处理，不在表单中插入状态行', async () => {
    const onPreviewTts = vi.fn().mockResolvedValue(undefined);
    renderPanel(onPreviewTts);

    fireEvent.click(screen.getByRole('button', { name: '测试播报' }));
    await waitFor(() => expect(onPreviewTts).toHaveBeenCalledOnce());

    const voiceField = screen.getByText('音色 ID').closest('label');
    expect(voiceField).not.toBeNull();
    expect(within(voiceField as HTMLElement).queryByText(/正在测试|测试播报已完成/)).not.toBeInTheDocument();
  });

  it('试听失败由控制器通知兜底，不向表单追加错误内容', async () => {
    const onPreviewTts = vi.fn().mockRejectedValue(new Error('语音服务不可用'));
    renderPanel(onPreviewTts);

    fireEvent.click(screen.getByRole('button', { name: '测试播报' }));
    await waitFor(() => expect(onPreviewTts).toHaveBeenCalledOnce());

    expect(screen.queryByText('语音服务不可用')).not.toBeInTheDocument();
  });
});
