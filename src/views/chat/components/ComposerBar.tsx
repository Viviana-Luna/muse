import { useCallback, useEffect, useLayoutEffect, useRef } from 'react';
import type { FormEvent, ReactNode, RefObject } from 'react';
import { Mic, MicOff, Square, Volume2, VolumeX } from 'lucide-react';

import type { VoiceRuntime } from '@/hooks/useVoiceRuntime';

interface ComposerBarProps {
  busy: boolean;
  canceling?: boolean;
  canCancel?: boolean;
  inputValue: string;
  voice: VoiceRuntime;
  sendIcon: ReactNode;
  sendDisabledReason?: string;
  visualizerCanvasRef?: RefObject<HTMLCanvasElement | null>;
  placeholder?: string;
  onInputValueChange: (value: string) => void;
  onSend: () => void | Promise<void>;
  onStartVoiceInput: () => void | Promise<void>;
  onToggleVoice: () => void;
  onStopVoice: () => void;
  onCancel?: () => void | Promise<void>;
}

export function ComposerBar({
  busy,
  canceling = false,
  canCancel = false,
  inputValue,
  voice,
  sendIcon,
  sendDisabledReason,
  visualizerCanvasRef,
  placeholder,
  onInputValueChange,
  onSend,
  onStartVoiceInput,
  onToggleVoice,
  onStopVoice,
  onCancel
}: ComposerBarProps) {
  const writeLocked = Boolean(sendDisabledReason);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  const resizeTextarea = useCallback(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    textarea.style.height = 'auto';
    const maxHeight = Number.parseFloat(window.getComputedStyle(textarea).maxHeight);
    const nextHeight = Number.isFinite(maxHeight)
      ? Math.min(textarea.scrollHeight, maxHeight)
      : textarea.scrollHeight;

    textarea.style.height = `${nextHeight}px`;
    textarea.style.overflowY = textarea.scrollHeight > nextHeight + 1 ? 'auto' : 'hidden';
  }, []);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  useLayoutEffect(() => {
    resizeTextarea();
  }, [inputValue, resizeTextarea]);

  useEffect(() => {
    window.addEventListener('resize', resizeTextarea);
    window.visualViewport?.addEventListener('resize', resizeTextarea);

    return () => {
      window.removeEventListener('resize', resizeTextarea);
      window.visualViewport?.removeEventListener('resize', resizeTextarea);
    };
  }, [resizeTextarea]);

  const submitMessage = () => {
    if (writeLocked || !inputValue.trim()) return;
    void onSend();
  };

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    submitMessage();
  };

  return (
    <section className="dialogue-panel composer-shell">
      <form
        className={`composer ${voice.listening || voice.voiceEnabled ? 'has-voice-activity' : ''}`}
        onSubmit={submit}
      >
        <div className="composer-input-layer">
          <textarea
            ref={textareaRef}
            value={inputValue}
            readOnly={writeLocked}
            placeholder={sendDisabledReason || placeholder || '对角色说点什么…'}
            onChange={(event) => onInputValueChange(event.target.value)}
            onKeyDown={(event) => {
              if (writeLocked) return;
              if (event.key === 'Enter' && !event.shiftKey) {
                event.preventDefault();
                submitMessage();
              }
            }}
          />
        </div>
        <div className="composer-control-layer">
          <div className="composer-status">
            <span>{sendDisabledReason || voice.status}</span>
            <canvas
              ref={visualizerCanvasRef}
              className="voice-visualizer composer-visualizer"
              width={220}
              height={26}
              aria-hidden="true"
            />
          </div>
          <div className="voice-panel">
            {(voice.speechRecognitionAvailable || voice.listening) && (
              <button
                type="button"
                className="voice-input"
                onClick={() => void onStartVoiceInput()}
                disabled={busy || writeLocked}
                aria-label={voice.listening ? '结束录音' : '语音输入'}
                title={
                  sendDisabledReason ||
                  (voice.listening
                    ? '点击结束录音并转写'
                    : '录音并通过语音识别服务转成文字')
                }
              >
                {voice.listening ? <MicOff aria-hidden="true" /> : <Mic aria-hidden="true" />}
                <span>{voice.listening ? '结束录音' : '语音输入'}</span>
              </button>
            )}
            {voice.ttsAvailable && (
              <button
                type="button"
                className="voice-toggle"
                onClick={onToggleVoice}
                disabled={writeLocked}
                aria-label={voice.voiceEnabled ? '关闭播报' : '语音播报'}
                title={sendDisabledReason || '用后端模型播报角色台词'}
              >
                {voice.voiceEnabled ? <VolumeX aria-hidden="true" /> : <Volume2 aria-hidden="true" />}
                <span>{voice.voiceEnabled ? '关闭播报' : '语音播报'}</span>
              </button>
            )}
            {voice.ttsAvailable && voice.voiceEnabled && (
              <button
                type="button"
                className="voice-stop"
                onClick={onStopVoice}
                aria-label="停止语音播报"
              >
                <Square aria-hidden="true" />
                <span>停止</span>
              </button>
            )}
          </div>
          <div className="composer-actions">
            {busy && canCancel ? (
              <button
                type="button"
                className="send-button stop-button"
                disabled={canceling}
                aria-label={canceling ? '正在停止回复' : '停止当前回复'}
                title={canceling ? '正在停止回复' : '停止当前回复'}
                onClick={() => void onCancel?.()}
              >
                {canceling ? '…' : '停'}
              </button>
            ) : (
              <button
                type="submit"
                className="send-button primary"
                disabled={busy || !inputValue.trim() || writeLocked}
                aria-label="发送"
                title={sendDisabledReason || '发送'}
              >
                {sendIcon}
              </button>
            )}
          </div>
        </div>
      </form>
    </section>
  );
}
