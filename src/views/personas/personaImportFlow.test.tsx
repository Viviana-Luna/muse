import { describe, expect, it } from 'vitest';

import { beginPersonaImport } from './personaImportFlow';

describe('beginPersonaImport', () => {
  it('先记录导入意图再打开角色库，不直接操作尚未挂载的文件输入框', () => {
    const sequence: string[] = [];

    beginPersonaImport({
      markPending: () => sequence.push('pending'),
      openLibrary: () => sequence.push('library')
    });

    expect(sequence).toEqual(['pending', 'library']);
  });
});
