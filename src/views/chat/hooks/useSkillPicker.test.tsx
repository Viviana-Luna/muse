import { act, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  listRuntimeSkills: vi.fn()
}));

vi.mock('@/api', () => api);

import { useSkillPicker } from './useSkillPicker';

type Picker = ReturnType<typeof useSkillPicker>;

function Harness({ personaId, current }: { personaId: string | null; current: { value?: Picker } }) {
  current.value = useSkillPicker(personaId);
  return null;
}

describe('useSkillPicker', () => {
  afterEach(() => vi.clearAllMocks());

  it('打开时读取运行时目录，并在角色变化后清除选择', async () => {
    api.listRuntimeSkills.mockResolvedValue({
      skills: [
        {
          name: 'skill-creator',
          description: '创建 Skill',
          revision: 'revision-1',
          source: 'builtin'
        }
      ],
      omitted_skill_count: 2
    });
    const current: { value?: Picker } = {};
    const view = render(<Harness personaId="persona-a" current={current} />);

    act(() => current.value?.openPicker());
    await waitFor(() => expect(current.value?.loading).toBe(false));
    expect(api.listRuntimeSkills).toHaveBeenCalledTimes(1);
    expect(current.value?.skills[0].name).toBe('skill-creator');
    expect(current.value?.omittedSkillCount).toBe(2);

    const skill = current.value!.skills[0];
    act(() => current.value?.selectSkill(skill));
    expect(current.value?.selectedSkill?.name).toBe('skill-creator');

    view.rerender(<Harness personaId="persona-b" current={current} />);
    expect(current.value?.selectedSkill).toBeNull();
    expect(current.value?.open).toBe(false);
  });
});
