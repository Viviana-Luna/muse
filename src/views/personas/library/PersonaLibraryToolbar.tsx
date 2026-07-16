import { Plus, Search, Upload } from 'lucide-react';

import type {
  PersonaStatusFilter,
  UsePersonaStateResult
} from '@/views/personas/hooks/usePersonaState';

interface PersonaLibraryToolbarProps {
  state: UsePersonaStateResult;
  busy: boolean;
  onImport: () => void;
  onCreate: () => void;
}

export function PersonaLibraryToolbar({
  state,
  busy,
  onImport,
  onCreate
}: PersonaLibraryToolbarProps) {
  return (
    <div className="persona-library-toolbar" aria-label="角色库筛选与操作">
      <label className="persona-library-search">
        <span className="sr-only">搜索角色</span>
        <Search aria-hidden="true" />
        <input
          type="search"
          value={state.search}
          placeholder="搜索角色名称、ID、作者"
          onChange={(event) => state.setSearch(event.target.value)}
        />
      </label>
      <label className="persona-library-filter">
        <span className="sr-only">角色状态</span>
        <select
          aria-label="角色状态"
          value={state.statusFilter}
          onChange={(event) =>
            state.setStatusFilter(event.target.value as PersonaStatusFilter)
          }
        >
          <option value="all">全部状态</option>
          <option value="active">当前角色</option>
          <option value="inactive">未启用</option>
        </select>
      </label>
      <span className="persona-library-toolbar-spacer" />
      <button type="button" className="persona-library-import-action" disabled={busy} onClick={onImport}>
        <Upload aria-hidden="true" />
        导入角色卡
      </button>
      <button type="button" className="persona-library-create-action" disabled={busy} onClick={onCreate}>
        <Plus aria-hidden="true" />
        创建角色
      </button>
    </div>
  );
}
