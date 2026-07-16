interface LoadingStateProps {
  label: string;
  description?: string;
}

interface SectionLoadingProps extends LoadingStateProps {
  variant?: 'page' | 'surface' | 'inline';
}

/** 冷启动期间覆盖完整 Web 视图，只展示品牌标志和真实加载说明。 */
export function AppLoadingScreen({
  label = '正在加载 Muse',
  description = '正在准备角色、会话与运行时状态…'
}: Partial<LoadingStateProps>) {
  return (
    <main
      className="app-loading-screen"
      role="status"
      aria-label={label}
      aria-live="polite"
      aria-atomic="true"
      aria-busy="true"
    >
      <div className="app-loading-content">
        <span className="app-loading-brand" aria-hidden="true">
          <img src="/assets/muse-logo.png" alt="" />
        </span>
        <strong>{label}</strong>
        {description && <span>{description}</span>}
      </div>
    </main>
  );
}

/** 页面、懒加载表面和行内区域共用的无装饰加载反馈。 */
export function SectionLoading({
  label,
  description,
  variant = 'page'
}: SectionLoadingProps) {
  return (
    <div
      className={`muse-section-loading is-${variant}`}
      role="status"
      aria-label={label}
      aria-live="polite"
      aria-atomic="true"
      aria-busy="true"
    >
      <span className="muse-loading-spinner" aria-hidden="true" />
      <span className="muse-loading-copy">
        <strong>{label}</strong>
        {description && <small>{description}</small>}
      </span>
    </div>
  );
}
