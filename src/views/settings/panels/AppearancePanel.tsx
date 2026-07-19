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
