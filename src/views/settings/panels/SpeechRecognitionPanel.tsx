import { SecretInput } from './SettingsFields';
import type { SettingsDialogProps } from '../types';

type SpeechRecognitionPanelProps = Pick<
  SettingsDialogProps,
  'settingsConfig' | 'busy' | 'maskedKeys' | 'updateSpeechRecognition'
>;

export function SpeechRecognitionPanel({
  settingsConfig,
  busy,
  maskedKeys,
  updateSpeechRecognition
}: SpeechRecognitionPanelProps) {
  const config = settingsConfig.speech_recognition;

  return (
    <div className="settings-panel-body">
      <section className="settings-module settings-module-compact">
        <div className="settings-card-head">
          <div>
            <span>OpenAI-compatible</span>
            <strong>语音识别服务</strong>
            <small>音频只发送到你配置的 `/v1/audio/transcriptions` 服务。</small>
          </div>
          <label className="settings-toggle">
            <input
              type="checkbox"
              checked={config.enabled}
              disabled={busy}
              onChange={(event) => updateSpeechRecognition('enabled', event.target.checked)}
            />
            <span>启用</span>
          </label>
        </div>
      </section>

      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>服务连接</h2>
            <p>配置转写 Provider、接口地址和 config.toml 凭据。</p>
          </div>
        </header>
        <div className="form-grid settings-module-grid">
          <label>
            服务类型
            <select
              value={config.provider}
              disabled={busy}
              onChange={(event) => updateSpeechRecognition('provider', event.target.value)}
            >
              <option value="openai_audio_transcriptions">OpenAI-compatible Transcriptions</option>
            </select>
          </label>
          <label>
            API Base
            <input
              value={config.api_base}
              disabled={busy}
              placeholder="https://api.openai.com/v1"
              onChange={(event) => updateSpeechRecognition('api_base', event.target.value)}
            />
          </label>
          <label className="wide">
            API Key
            <SecretInput
              configured={!!maskedKeys.asr}
              value={config.api_key ?? ''}
              placeholder={maskedKeys.asr ? '已配置，可留空保持' : '未配置'}
              disabled={busy}
              onChange={(event) => updateSpeechRecognition('api_key', event.target.value || null)}
            />
          </label>
        </div>
      </section>

      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>识别参数</h2>
            <p>指定转写模型、语言提示与返回格式。</p>
          </div>
        </header>
        <div className="settings-inline-fields">
          <label>
            模型
            <input
              value={config.model}
              disabled={busy}
              placeholder="whisper-1"
              onChange={(event) => updateSpeechRecognition('model', event.target.value)}
            />
          </label>
          <label>
            语言
            <input
              value={config.language}
              disabled={busy}
              placeholder="zh，可留空自动识别"
              onChange={(event) => updateSpeechRecognition('language', event.target.value)}
            />
          </label>
          <label>
            返回格式
            <select
              value={config.response_format}
              disabled={busy}
              onChange={(event) => updateSpeechRecognition('response_format', event.target.value)}
            >
              <option value="json">JSON</option>
            </select>
          </label>
        </div>
      </section>
    </div>
  );
}
