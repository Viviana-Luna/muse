import type { MotionLevel, SettingsDialogProps } from '../types';

type AppearancePanelProps = Pick<
  SettingsDialogProps,
  'appearanceSettings' | 'setAppearanceSettings'
>;

const TRANSLUCENT_BACKGROUND_OPACITY = 0.72;

const BACKGROUND_OPTIONS = [
  {
    value: 'solid',
    label: '实色背景',
    description: '保持完整画布底色，提供稳定的高对比度阅读体验。'
  },
  {
    value: 'translucent',
    label: '半透明背景',
    description: '在 Windows 11 中透出桌面背景，浮岛继续保持独立材质。'
  }
] as const;

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
  const backgroundMode = appearanceSettings.backgroundOpacity < 1 ? 'translucent' : 'solid';

  return (
    <div className="settings-panel-body">
      <section className="settings-module">
        <header className="settings-module-head">
          <div>
            <h2>背景材质</h2>
            <p>控制浮岛之间的应用画布是否透出 Windows 桌面；更改会立即预览。</p>
          </div>
        </header>
        <div
          className="segmented-control background-material-options"
          role="group"
          aria-label="背景材质"
        >
          {BACKGROUND_OPTIONS.map((option) => (
            <button
              type="button"
              key={option.value}
              className={backgroundMode === option.value ? 'active' : ''}
              aria-pressed={backgroundMode === option.value}
              onClick={() =>
                setAppearanceSettings((state) => ({
                  ...state,
                  backgroundOpacity:
                    option.value === 'solid' ? 1 : TRANSLUCENT_BACKGROUND_OPACITY
                }))
              }
            >
              <strong>{option.label}</strong>
              <small>{option.description}</small>
            </button>
          ))}
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
