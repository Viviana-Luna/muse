import type { AppearanceBackgroundTheme, AppearanceTheme } from '@/types';
import type { MotionLevel, SettingsDialogProps } from '../types';

type AppearancePanelProps = Pick<
  SettingsDialogProps,
  'appearanceSettings' | 'setAppearanceSettings'
>;

const TRANSLUCENT_BACKGROUND_OPACITY = 0.72;

const THEME_OPTIONS: Array<{
  value: AppearanceTheme;
  label: string;
  description: string;
}> = [
  {
    value: 'system',
    label: '跟随系统',
    description: '自动匹配 Windows 当前的亮色或深色模式。'
  },
  { value: 'dark', label: '深色主题', description: '始终使用深色浮岛与浅色文字。' },
  { value: 'light', label: '浅色主题', description: '始终使用浅色浮岛与深色文字。' }
];

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

const BACKGROUND_THEME_OPTIONS: Array<{
  value: AppearanceBackgroundTheme;
  label: string;
  description: string;
}> = [
  {
    value: 'dark',
    label: '深色背景',
    description: '整扇窗口使用深色连续底层，浮岛主题保持独立。'
  },
  {
    value: 'light',
    label: '浅色背景',
    description: '整扇窗口使用浅色连续底层，不改变浮岛与文字配色。'
  }
];

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
            <h2>界面主题</h2>
            <p>选择浮岛、文字与控件的明暗；角色强调色不会覆盖这里的选择。</p>
          </div>
        </header>
        <div className="segmented-control" role="group" aria-label="界面主题">
          {THEME_OPTIONS.map((option) => (
            <button
              type="button"
              key={option.value}
              className={appearanceSettings.theme === option.value ? 'active' : ''}
              aria-pressed={appearanceSettings.theme === option.value}
              onClick={() =>
                setAppearanceSettings((state) => ({ ...state, theme: option.value }))
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
            <h2>背景明暗</h2>
            <p>只控制浮岛下方的一整块窗口背景，与界面主题分别保存。</p>
          </div>
        </header>
        <div className="segmented-control" role="group" aria-label="背景明暗">
          {BACKGROUND_THEME_OPTIONS.map((option) => (
            <button
              type="button"
              key={option.value}
              className={appearanceSettings.backgroundTheme === option.value ? 'active' : ''}
              aria-pressed={appearanceSettings.backgroundTheme === option.value}
              onClick={() =>
                setAppearanceSettings((state) => ({
                  ...state,
                  backgroundTheme: option.value
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
