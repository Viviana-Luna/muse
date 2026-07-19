import { useEffect, useId, useState } from 'react';
import { Bot, ChevronDown, ShieldCheck, TriangleAlert, UserRoundCheck } from 'lucide-react';

import { ConfirmDialog } from '@/components/feedback/ConfirmDialog';
import type { RuntimeApprovalModePreset } from '@/types';

interface ApprovalModePickerProps {
  value: RuntimeApprovalModePreset;
  switching?: boolean;
  disabled?: boolean;
  disabledReason?: string;
  onChange: (value: RuntimeApprovalModePreset) => void | Promise<void>;
}

const MODE_LABELS: Record<RuntimeApprovalModePreset, string> = {
  manual: '手动审批',
  auto: 'AUTO 模式',
  yolo: 'YOLO 模式'
};

export function ApprovalModePicker({
  value,
  switching = false,
  disabled = false,
  disabledReason,
  onChange
}: ApprovalModePickerProps) {
  const [open, setOpen] = useState(false);
  const titleId = useId();
  const blocked = disabled || switching;

  useEffect(() => {
    if (blocked) setOpen(false);
  }, [blocked]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [open]);

  const select = async (preset: RuntimeApprovalModePreset) => {
    if (preset === value || switching) {
      setOpen(false);
      return;
    }
    await onChange(preset);
    setOpen(false);
  };

  return (
    <>
      <button
        type="button"
        className={`approval-mode-trigger is-${value}`}
        disabled={blocked}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls="approval-mode-picker"
        title={disabledReason || '切换当前会话的审批模式'}
        onClick={() => setOpen((current) => !current)}
      >
        {value === 'manual' ? (
          <UserRoundCheck aria-hidden="true" />
        ) : value === 'auto' ? (
          <Bot aria-hidden="true" />
        ) : (
          <TriangleAlert aria-hidden="true" />
        )}
        <span>{switching ? '切换中…' : MODE_LABELS[value]}</span>
        <ChevronDown aria-hidden="true" />
      </button>

      {open && (
        <div className="approval-mode-layer">
          <button
            type="button"
            className="approval-mode-scrim"
            aria-label="关闭审批模式选择器"
            onClick={() => setOpen(false)}
          />
          <section
            id="approval-mode-picker"
            className="approval-mode-drawer"
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
          >
            <header>
              <div>
                <span>当前会话</span>
                <h2 id={titleId}>选择审批模式</h2>
                <p>切换只影响后续新回合；MCP 继续遵守独立安全策略。</p>
              </div>
              <button type="button" onClick={() => setOpen(false)} aria-label="关闭审批模式选择器">
                关闭
              </button>
            </header>
            <div className="approval-mode-options">
              <button
                type="button"
                className={value === 'manual' ? 'is-active' : ''}
                disabled={switching}
                onClick={() => void select('manual')}
              >
                <UserRoundCheck aria-hidden="true" />
                <strong>手动审批</strong>
                <small>需要审批的动作由你确认，权限限制在当前工作区。</small>
              </button>
              <button
                type="button"
                className={value === 'auto' ? 'is-active is-auto' : 'is-auto'}
                disabled={switching}
                onClick={() => void select('auto')}
              >
                <Bot aria-hidden="true" />
                <strong>AUTO 模式</strong>
                <small>由无工具、无角色、无 Skill 的独立审查器按风险决定；异常时转人工。</small>
              </button>
              <ConfirmDialog
                title="开启当前会话的 YOLO 模式？"
                description="YOLO 会跳过普通工具审批并允许访问工作区外路径。它只在本次应用运行期有效，重启或恢复旧会话会回到手动审批；MCP 不受此模式放宽。"
                confirmLabel="开启 YOLO"
                cancelLabel="继续保持限制"
                tone="danger"
                onConfirm={() => void select('yolo')}
              >
                <button
                  type="button"
                  className={value === 'yolo' ? 'is-active is-yolo' : 'is-yolo'}
                  disabled={switching}
                >
                  <TriangleAlert aria-hidden="true" />
                  <strong>YOLO 模式</strong>
                  <small>跳过普通审批并启用完全访问；可能造成不可逆的数据或系统变更。</small>
                </button>
              </ConfirmDialog>
            </div>
            <footer>
              <ShieldCheck aria-hidden="true" />
              <span>AUTO 不是免审；只有 YOLO 会关闭普通审批边界。</span>
            </footer>
          </section>
        </div>
      )}
    </>
  );
}
