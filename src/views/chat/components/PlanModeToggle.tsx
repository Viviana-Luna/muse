import { ListChecks, LoaderCircle } from 'lucide-react';

import type { RuntimeToolPreset } from '@/types';

export type PlanModeChoice = 'focus_build' | 'focus_plan';

interface PlanModeToggleProps {
  value: RuntimeToolPreset;
  disabled?: boolean;
  switching?: boolean;
  disabledReason?: string;
  onChange: (value: PlanModeChoice) => void | Promise<void>;
}

export function PlanModeToggle({
  value,
  disabled = false,
  switching = false,
  disabledReason,
  onChange
}: PlanModeToggleProps) {
  const active = value === 'focus_plan';
  const actionLabel = active ? '退出计划模式' : '进入计划模式';

  return (
    <button
      type="button"
      className={`plan-mode-trigger${active ? ' is-selected' : ''}`}
      disabled={disabled || switching}
      aria-pressed={active}
      aria-label={actionLabel}
      title={disabledReason || actionLabel}
      onClick={() => void onChange(active ? 'focus_build' : 'focus_plan')}
    >
      {switching ? (
        <LoaderCircle className="is-spinning" aria-hidden="true" />
      ) : (
        <ListChecks aria-hidden="true" />
      )}
      <span>{active ? '计划中' : '计划'}</span>
    </button>
  );
}
