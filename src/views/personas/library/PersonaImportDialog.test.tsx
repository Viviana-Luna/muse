import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
import type { ChangeEvent } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { PersonaCard } from '@/types';
import type { usePersonaController } from '@/views/personas/hooks/usePersonaController';
import { usePersonaState } from '@/views/personas/hooks/usePersonaState';
import { PersonaImportDialog } from './PersonaImportDialog';

const validCard: PersonaCard = {
  schema_version: '1.0',
  exported_at: '2026-07-12T00:00:00Z',
  export_level: 'with_visual_pack_ref',
  persona: {
    id: 'alice',
    name: '爱丽丝',
    summary: '沉稳可靠的角色',
    character_profile: '沉稳',
    world_profile: '现实',
    scenario: '',
    system_prompt: '保持角色。',
    style: '',
    roleplay_style: 'light_narration',
    dialogue_examples: '',
    author_note: '',
    opening_message: '你好。',
    tool_policy: { mode: 'inherit', allowed_tools: [] },
    skill_policy: { mode: 'inherit', allowed_skills: [] },
    mcp_policy: { mode: 'inherit', allowed_servers: [] },
    preferred_model_ref: null,
    preferred_voice_id: null,
    feature_policy: { emotion_persistence_enabled: true },
    default_visual_pack_id: 'visual-alice',
    author: 'Muse',
    version: '1.0.0',
    notes: ''
  },
  visual_pack: {
    id: 'visual-alice',
    name: '爱丽丝展示包',
    portrait_path: '/assets/alice-portrait.png',
    background_path: '/assets/alice-background.png',
    avatar_path: '/assets/alice-avatar.png',
    theme_color: '#887766',
    theme_mode: 'dark',
    layout_mode: 'default',
    portrait_frame: 'portrait',
    portrait_fit: 'cover',
    portrait_position_x: 50,
    portrait_position_y: 50,
    portrait_scale: 100,
    fallback_text: '',
    version: '1.0.0',
    notes: ''
  }
};

const validCardText = JSON.stringify(validCard);

function ImportHarness() {
  const state = usePersonaState();
  const [error, setError] = useState('');

  async function handlePersonaCardFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) return;
    state.setImportFileName(file.name);
    state.setImportText(await file.text());
    setError('');
  }

  const controller = {
    personaImportError: error,
    setPersonaImportError: setError,
    handlePersonaCardFile,
    handleImport: () => setError('角色卡字段校验失败，请检查后重试。')
  } as unknown as ReturnType<typeof usePersonaController>;

  return (
    <main className="runtime-shell">
      <button type="button" onClick={state.openImportDialog}>打开导入</button>
      <output data-testid="draft-state">
        {JSON.stringify({
          open: state.importDialogOpen,
          fileName: state.importFileName,
          text: state.importText,
          strategy: state.importConflictStrategy,
          activate: state.activateAfterImport
        })}
      </output>
      <PersonaImportDialog state={state} controller={controller} busy={false} />
    </main>
  );
}

function createCardFile(contents = validCardText) {
  const file = new File([contents], 'alice.muse-role-card.json', {
    type: 'application/json'
  });
  Object.defineProperty(file, 'text', {
    configurable: true,
    value: vi.fn().mockResolvedValue(contents)
  });
  return file;
}

async function openDialog() {
  const trigger = screen.getByRole('button', { name: '打开导入' });
  trigger.focus();
  fireEvent.click(trigger);
  await screen.findByRole('dialog', { name: '导入角色卡' });
  return trigger;
}

beforeEach(() => {
  vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
    callback(0);
    return 1;
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('PersonaImportDialog', () => {
  it('文件按钮只打开受控文件输入，有效 JSON 读取后显示预览', async () => {
    render(<ImportHarness />);
    await openDialog();
    const dialog = screen.getByRole('dialog', { name: '导入角色卡' });
    const fileInput = dialog.querySelector<HTMLInputElement>('input[type="file"]');
    expect(fileInput).not.toBeNull();
    expect(fileInput).toHaveAttribute('accept', 'application/json,.json,.muse-role-card.json');

    const inputClick = vi.spyOn(fileInput!, 'click').mockImplementation(() => undefined);
    fireEvent.click(screen.getByRole('button', { name: /^选择角色卡文件/ }));
    expect(inputClick).toHaveBeenCalledOnce();

    fireEvent.change(fileInput!, { target: { files: [createCardFile()] } });
    expect(await screen.findByText('爱丽丝')).toBeInTheDocument();
    expect(screen.getByText('沉稳可靠的角色')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '导入角色卡' })).toBeEnabled();
    expect(screen.getByRole('button', { name: /alice\.muse-role-card\.json/ })).toBeInTheDocument();
  });

  it('无效 JSON 显示局部提示并禁止提交', async () => {
    render(<ImportHarness />);
    await openDialog();
    const textarea = document.querySelector<HTMLTextAreaElement>('.persona-import-advanced textarea');
    expect(textarea).not.toBeNull();

    fireEvent.change(textarea!, { target: { value: '{ invalid json' } });

    expect(screen.getByText('当前内容尚不能识别为 Muse 角色卡，请检查 JSON。')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '导入角色卡' })).toBeDisabled();
  });

  it('高级粘贴会更新预览并清除先前文件名', async () => {
    render(<ImportHarness />);
    await openDialog();
    const fileInput = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    fireEvent.change(fileInput, { target: { files: [createCardFile()] } });
    await screen.findByText('爱丽丝');

    const details = screen.getByText('高级导入：粘贴 JSON').closest('details')!;
    details.open = true;
    fireEvent(details, new Event('toggle'));
    const textarea = details.querySelector('textarea')!;
    const pasted = JSON.stringify({
      ...validCard,
      persona: { ...validCard.persona, id: 'bob', name: '鲍勃' }
    });
    fireEvent.change(textarea, { target: { value: pasted } });

    expect(await screen.findByText('鲍勃')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /alice\.muse-role-card\.json/ })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^选择角色卡文件/ })).toBeInTheDocument();
  });

  it('提交错误会展开高级区域并把焦点移到可见错误', async () => {
    render(<ImportHarness />);
    await openDialog();
    const textarea = document.querySelector<HTMLTextAreaElement>('.persona-import-advanced textarea')!;
    fireEvent.change(textarea, { target: { value: validCardText } });

    fireEvent.click(screen.getByRole('button', { name: '导入角色卡' }));

    const error = await screen.findByRole('alert');
    await waitFor(() => expect(error).toHaveFocus());
    expect(error).toHaveTextContent('角色卡字段校验失败，请检查后重试。');
    expect(error.closest('section')?.querySelector('details')).toHaveAttribute('open');
  });

  it('取消会重置导入草稿并把焦点恢复给打开按钮', async () => {
    render(<ImportHarness />);
    const trigger = await openDialog();
    const textarea = document.querySelector<HTMLTextAreaElement>('.persona-import-advanced textarea')!;
    fireEvent.change(textarea, { target: { value: validCardText } });
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'overwrite' } });
    fireEvent.click(screen.getByRole('checkbox', { name: /导入后启用/ }));

    fireEvent.click(screen.getByRole('button', { name: '取消' }));

    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(trigger).toHaveFocus();
    expect(screen.getByTestId('draft-state')).toHaveTextContent(
      JSON.stringify({ open: false, fileName: '', text: '', strategy: 'rename', activate: true })
    );

    fireEvent.click(trigger);
    await screen.findByRole('dialog', { name: '导入角色卡' });
    expect(screen.getByRole('combobox')).toHaveValue('rename');
    expect(screen.getByRole('checkbox', { name: /导入后启用/ })).toBeChecked();
    expect(document.querySelector<HTMLTextAreaElement>('.persona-import-advanced textarea')).toHaveValue('');
  });
});
