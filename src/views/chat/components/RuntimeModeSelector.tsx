import { useEffect, useId, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import {
  Check,
  ChevronDown,
  Coffee,
  Hammer,
  ListChecks,
  LoaderCircle
} from 'lucide-react';

import type { RuntimeToolPreset } from '@/types';

export type RuntimeModeChoice = 'daily' | 'focus_build' | 'focus_plan';

const MODE_OPTIONS = [
  {
    value: 'daily',
    label: '日常',
    description: '轻量对话与基础助手工具',
    icon: Coffee
  },
  {
    value: 'focus_build',
    label: '工作',
    description: '开放执行与写入工具',
    icon: Hammer
  },
  {
    value: 'focus_plan',
    label: '计划',
    description: '只读分析与任务规划',
    icon: ListChecks
  }
] as const;

interface RuntimeModeSelectorProps {
  value: RuntimeToolPreset;
  disabled?: boolean;
  switching?: boolean;
  disabledReason?: string;
  onChange: (value: RuntimeModeChoice) => void | Promise<void>;
}

function normalizeMode(value: RuntimeToolPreset): RuntimeModeChoice {
  return MODE_OPTIONS.some((option) => option.value === value)
    ? (value as RuntimeModeChoice)
    : 'daily';
}

export function RuntimeModeSelector({
  value,
  disabled = false,
  switching = false,
  disabledReason,
  onChange
}: RuntimeModeSelectorProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const menuId = useId();
  const normalizedValue = normalizeMode(value);
  const current = MODE_OPTIONS.find((option) => option.value === normalizedValue)!;
  const CurrentIcon = current.icon;

  const closeAndRestoreFocus = () => {
    setOpen(false);
    window.requestAnimationFrame(() => triggerRef.current?.focus());
  };

  useEffect(() => {
    if (!open) return;
    const currentIndex = MODE_OPTIONS.findIndex((option) => option.value === normalizedValue);
    optionRefs.current[currentIndex]?.focus();

    const handlePointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener('pointerdown', handlePointerDown);
    return () => document.removeEventListener('pointerdown', handlePointerDown);
  }, [normalizedValue, open]);

  useEffect(() => {
    if (disabled && open) setOpen(false);
  }, [disabled, open]);

  const handleMenuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const currentIndex = optionRefs.current.findIndex(
      (option) => option === document.activeElement
    );
    let nextIndex: number | null = null;
    if (event.key === 'ArrowDown') nextIndex = (currentIndex + 1) % MODE_OPTIONS.length;
    if (event.key === 'ArrowUp') {
      nextIndex = (currentIndex - 1 + MODE_OPTIONS.length) % MODE_OPTIONS.length;
    }
    if (event.key === 'Home') nextIndex = 0;
    if (event.key === 'End') nextIndex = MODE_OPTIONS.length - 1;
    if (event.key === 'Escape') {
      event.preventDefault();
      closeAndRestoreFocus();
      return;
    }
    if (nextIndex !== null) {
      event.preventDefault();
      optionRefs.current[nextIndex]?.focus();
    }
  };

  return (
    <div className="runtime-mode-control" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={`runtime-mode-trigger${open ? ' is-open' : ''}`}
        disabled={disabled || switching}
        aria-label={`运行模式：${current.label}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={menuId}
        title={disabledReason || `当前为${current.label}模式`}
        onClick={() => setOpen((currentOpen) => !currentOpen)}
      >
        {switching ? (
          <LoaderCircle className="is-spinning" aria-hidden="true" />
        ) : (
          <CurrentIcon aria-hidden="true" />
        )}
        <span>{current.label}</span>
        <ChevronDown className="runtime-mode-chevron" aria-hidden="true" />
      </button>

      {open && (
        <div
          id={menuId}
          className="runtime-mode-menu"
          role="menu"
          aria-label="选择运行模式"
          onKeyDown={handleMenuKeyDown}
        >
          <div className="runtime-mode-menu-heading">
            <strong>运行模式</strong>
            <span>应用于后续新回合</span>
          </div>
          {MODE_OPTIONS.map((option, index) => {
            const Icon = option.icon;
            const selected = option.value === normalizedValue;
            return (
              <button
                key={option.value}
                ref={(element) => {
                  optionRefs.current[index] = element;
                }}
                type="button"
                className={selected ? 'is-selected' : ''}
                role="menuitemradio"
                aria-checked={selected}
                onClick={() => {
                  setOpen(false);
                  triggerRef.current?.focus();
                  if (!selected) void onChange(option.value);
                }}
              >
                <span className="runtime-mode-option-icon">
                  <Icon aria-hidden="true" />
                </span>
                <span className="runtime-mode-option-copy">
                  <strong>{option.label}</strong>
                  <small>{option.description}</small>
                </span>
                {selected && <Check className="runtime-mode-check" aria-hidden="true" />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
