import { NumericSliderField } from './SettingsFields';
import type { MotionLevel, SettingsDialogProps } from '../types';

type AppearancePanelProps = Pick<
  SettingsDialogProps,
  'appearanceSettings' | 'setAppearanceSettings'
>;

const MOTION_OPTIONS: Array<{
  value: MotionLevel;
  label: string;
  description: string;
}> = [
  { value: 'full', label: '完全动效', description: '保留舞台、波形与弹窗动效。' },
  { value: 'reduced', label: '减弱动效', description: '降低循环动画和过渡强度。' },
  { value: 'none', label: '无动效', description: '关闭非必要动画与过渡。' }
];

export function AppearancePanel({ appearanceSettings, setAppearanceSettings }: AppearancePanelProps) {
  return (
    <div className="settings-panel-body">
      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>舞台背景</h2>
            <p>控制角色立绘背景的清晰度与可见程度。</p>
          </div>
        </header>
        <div className="form-grid settings-module-grid">
          <NumericSliderField
            label="立绘背景模糊"
            value={appearanceSettings.backgroundBlur}
            min={0}
            max={30}
            step={1}
            unit="px"
            className="wide"
            onChange={(value) =>
              setAppearanceSettings((state) => ({ ...state, backgroundBlur: value }))
            }
          />
          <NumericSliderField
            label="背景可见度"
            value={appearanceSettings.backgroundOpacity}
            min={0.2}
            max={1}
            step={0.1}
            className="wide"
            onChange={(value) =>
              setAppearanceSettings((state) => ({ ...state, backgroundOpacity: value }))
            }
          />
        </div>
      </section>

      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>界面动效</h2>
            <p>选择适合当前设备与使用习惯的动画强度。</p>
          </div>
        </header>
        <div className="segmented-control wide" role="group" aria-label="动画速度">
          {MOTION_OPTIONS.map((option) => (
            <button
              type="button"
              key={option.value}
              className={appearanceSettings.motionLevel === option.value ? 'active' : ''}
              onClick={() =>
                setAppearanceSettings((state) => ({ ...state, motionLevel: option.value }))
              }
            >
              <strong>{option.label}</strong>
              <small>{option.description}</small>
            </button>
          ))}
        </div>
      </section>
    </div>
  );
}
