import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';
import { RefreshCw, X } from 'lucide-react';

import { useModalAccessibility } from '@/hooks/useModalAccessibility';

interface LazySurfaceBoundaryProps {
  children: ReactNode;
  label: string;
  onRetry: () => void;
  onClose?: () => void;
}

interface LazySurfaceBoundaryState {
  error: Error | null;
}

const keepSurfaceOpen = () => undefined;

function ModalAccessibilitySurface({
  children,
  onClose
}: {
  children: ReactNode;
  onClose?: () => void;
}) {
  const modalRef = useModalAccessibility<HTMLDivElement>(true, onClose ?? keepSurfaceOpen);

  return (
    <div ref={modalRef} tabIndex={-1} data-modal-accessibility-surface="true">
      {children}
    </div>
  );
}

/** 为独立懒加载工作区提供局部恢复，避免 chunk 失败被误报成首页资源失败。 */
export class LazySurfaceBoundary extends Component<
  LazySurfaceBoundaryProps,
  LazySurfaceBoundaryState
> {
  state: LazySurfaceBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): LazySurfaceBoundaryState {
    return { error };
  }

  componentDidCatch(_error: Error, _info: ErrorInfo) {
    // 错误正文只留在当前工作区，避免向全局日志输出可能含本机路径的 chunk URL。
  }

  private retry = () => {
    this.props.onRetry();
    this.setState({ error: null });
  };

  render() {
    if (!this.state.error) {
      return (
        <ModalAccessibilitySurface onClose={this.props.onClose}>
          {this.props.children}
        </ModalAccessibilitySurface>
      );
    }
    return (
      <ModalAccessibilitySurface onClose={this.props.onClose}>
        <section className="modal-shell" role="alert" aria-label={`${this.props.label}加载失败`}>
          <div className="lazy-surface-error">
            <header>
              <span>
                <small>界面资源暂不可用</small>
                <strong>{this.props.label}加载失败</strong>
              </span>
              {this.props.onClose && (
                <button type="button" onClick={this.props.onClose} aria-label={`关闭${this.props.label}`}>
                  <X aria-hidden="true" />
                </button>
              )}
            </header>
            <p>可能是本地界面资源短暂读取失败，请在当前位置重试。</p>
            <button type="button" className="primary" onClick={this.retry}>
              <RefreshCw aria-hidden="true" />
              重新加载
            </button>
          </div>
        </section>
      </ModalAccessibilitySurface>
    );
  }
}
