import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { SecretInput } from './SettingsFields';

describe('SecretInput', () => {
  it('已保存密钥保持空值，不会把掩码当作可显示的密钥', () => {
    render(
      <SecretInput
        configured
        value=""
        placeholder="已配置，可留空保持"
        onChange={() => undefined}
      />
    );

    const input = screen.getByPlaceholderText('已配置，可留空保持');
    const toggle = screen.getByRole('button', { name: '已保存密钥不可读取' });
    expect(input).toHaveValue('');
    expect(input).toHaveAttribute('type', 'password');
    expect(toggle).toBeDisabled();
  });

  it('输入新密钥后才允许切换可见性', () => {
    function Wrapper() {
      return (
        <SecretInput
          configured
          value="new-secret"
          onChange={() => undefined}
        />
      );
    }

    render(<Wrapper />);
    const toggle = screen.getByRole('button', { name: '显示密钥' });
    expect(toggle).toBeEnabled();
    fireEvent.click(toggle);
    expect(screen.getByRole('button', { name: '隐藏密钥' })).toBeVisible();
    expect(screen.getByDisplayValue('new-secret')).toHaveAttribute('type', 'text');
  });
});
