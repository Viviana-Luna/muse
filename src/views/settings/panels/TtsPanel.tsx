import { NumericSliderField, SecretInput } from './SettingsFields';
import type { SettingsDialogProps } from '../types';

type TtsPanelProps = Pick<
  SettingsDialogProps,
  'settingsConfig' | 'busy' | 'maskedKeys' | 'setSettingsConfig' | 'onPreviewTts'
>;

export function TtsPanel({
  settingsConfig,
  busy,
  maskedKeys,
  setSettingsConfig,
  onPreviewTts
}: TtsPanelProps) {
  function updateTts<K extends keyof SettingsDialogProps['settingsConfig']['tts']>(
    key: K,
    value: SettingsDialogProps['settingsConfig']['tts'][K]
  ) {
    setSettingsConfig((state) =>
      state
        ? {
            ...state,
            tts: {
              ...state.tts,
              [key]: value
            }
          }
        : state
    );
  }

  async function handlePreview() {
    try {
      await onPreviewTts();
    } catch {
      // 控制器已经通过统一通知反馈错误；这里只负责收口 Promise，避免重复展示。
    }
  }

  return (
    <div className="settings-panel-body">
      <section className="settings-module settings-module-compact">
        <label className="settings-toggle wide">
          <input
            type="checkbox"
            checked={settingsConfig.tts.enabled}
            onChange={(event) => updateTts('enabled', event.target.checked)}
          />
          <span>
            <strong>启用语音合成</strong>
            <small>通过 OpenAI-compatible `/v1/audio/speech` 服务合成音频。</small>
          </span>
        </label>
      </section>

      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>服务连接</h2>
            <p>配置兼容接口、访问凭据与语音模型。</p>
          </div>
        </header>
        <div className="form-grid settings-module-grid">
          <label>
            Provider
            <select
              value={settingsConfig.tts.provider}
              onChange={(event) => updateTts('provider', event.target.value)}
            >
              <option value="openai_audio_speech">OpenAI-compatible Speech</option>
            </select>
          </label>
          <label>
            Base URL
            <input
              value={settingsConfig.tts.api_base}
              placeholder="https://api.openai.com/v1"
              onChange={(event) => updateTts('api_base', event.target.value)}
            />
          </label>
          <label>
            API Key
            <SecretInput
              configured={!!maskedKeys.tts}
              value={settingsConfig.tts.api_key ?? ''}
              placeholder={maskedKeys.tts ? '已配置，可留空保持' : '未配置'}
              onChange={(event) => updateTts('api_key', event.target.value || null)}
            />
          </label>
          <label>
            模型名称
            <input
              value={settingsConfig.tts.model}
              placeholder="tts-1"
              onChange={(event) => updateTts('model', event.target.value)}
            />
          </label>
        </div>
      </section>

      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>声音与输出</h2>
            <p>调整音色、编码格式和播报速度，并在保存后试听。</p>
          </div>
        </header>
        <div className="form-grid settings-module-grid">
          <label className="wide">
            音色 ID
            <div className="model-input-row">
              <input
                value={settingsConfig.tts.voice_id}
                placeholder="alloy"
                onChange={(event) => updateTts('voice_id', event.target.value)}
              />
              <button type="button" onClick={() => void handlePreview()} disabled={busy}>
                测试播报
              </button>
            </div>
            <span className="tts-preview-meter" aria-hidden="true">
              <i />
              <i />
              <i />
              <i />
              <i />
            </span>
          </label>
          <label>
            输出格式
            <input
              value={settingsConfig.tts.response_format}
              placeholder="mp3"
              onChange={(event) => updateTts('response_format', event.target.value)}
            />
          </label>
          <NumericSliderField
            label="语速"
            value={settingsConfig.tts.speed}
            min={0.25}
            max={4}
            step={0.05}
            unit="x"
            onChange={(value) => updateTts('speed', value)}
          />
          <div className="model-meta-row wide">
            <span className="field-hint">
              本地 TTS 模型需要先由用户启动为 OpenAI 兼容语音服务，再填入 Base URL；应用本体不再内置 TTS 推理。
            </span>
          </div>
        </div>
      </section>
    </div>
  );
}
