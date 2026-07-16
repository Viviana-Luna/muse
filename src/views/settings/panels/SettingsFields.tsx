import { useEffect, useState, type InputHTMLAttributes, type ReactNode } from 'react';

interface SecretInputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'type'> {
  configured?: boolean;
}

interface NumericSliderFieldProps {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  hint?: ReactNode;
  unit?: string;
  className?: string;
  disabled?: boolean;
  onChange: (value: number) => void;
}

export function SecretInput({
  configured = false,
  autoComplete = 'off',
  ...props
}: SecretInputProps) {
  const [visible, setVisible] = useState(false);
  const value = typeof props.value === 'string' ? props.value : '';
  const looksMasked = value.includes('****') || value.includes('••••');
  const canToggle = value.trim().length > 0 && !looksMasked && !props.disabled;

  useEffect(() => {
    if (!canToggle && visible) setVisible(false);
  }, [canToggle, visible]);

  return (
    <div className="secret-input">
      <input {...props} type={visible ? 'text' : 'password'} autoComplete={autoComplete} />
      <button
        type="button"
        className="secret-input-toggle"
        disabled={!canToggle}
        onClick={() => setVisible((value) => !value)}
        aria-label={canToggle ? (visible ? '隐藏密钥' : '显示密钥') : '已保存密钥不可读取'}
        title={canToggle ? (visible ? '隐藏密钥' : '显示密钥') : '已保存密钥不可读取'}
      >
        {canToggle ? (visible ? '隐藏' : '显示') : configured ? '已保存' : '显示'}
      </button>
      {configured && <span className="status-pill success">已配置，可留空保持</span>}
    </div>
  );
}

export function NumericSliderField({
  label,
  value,
  min,
  max,
  step,
  hint,
  unit = '',
  className,
  disabled = false,
  onChange
}: NumericSliderFieldProps) {
  function formatValue(nextValue: number) {
    const [, decimals = ''] = step.toString().split('.');
    return nextValue.toFixed(decimals.length);
  }

  function parseValue(raw: string) {
    const parsed = Number.parseFloat(raw);
    if (Number.isNaN(parsed)) return min;
    return Math.min(max, Math.max(min, parsed));
  }

  return (
    <label className={`range-field ${className ?? ''}`.trim()}>
      <span className="range-field-head">
        <strong>{label}</strong>
        <em>{`${formatValue(value)}${unit}`}</em>
      </span>
      <span className="range-input-row">
        <input
          type="range"
          disabled={disabled}
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(event) => onChange(parseValue(event.target.value))}
        />
        <input
          className="range-number"
          type="number"
          disabled={disabled}
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(event) => onChange(parseValue(event.target.value))}
        />
      </span>
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}
