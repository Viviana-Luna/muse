import { useCallback, useRef, useState } from 'react';

export interface RevisionSnapshot {
  state_revision: number;
}

/**
 * 无 revision 的旧查询接口通过前后两次事实快照获得一致性窗口。
 * 中途发生 mutation 时丢弃本轮数据并重试，避免把 A 会话结果写进 B 会话。
 */
export async function loadAtStableRevision<T, S extends RevisionSnapshot>(options: {
  load: (stableState: S) => Promise<T>;
  readState: () => Promise<S>;
  currentRevision: () => number;
  acceptRevision: (revision: number) => boolean;
  attempts?: number;
}): Promise<{ value: T; state: S }> {
  const attempts = Math.max(1, options.attempts ?? 3);
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const before = await options.readState();
    if (before.state_revision < options.currentRevision()) continue;
    // 业务读取必须基于同一个 before 快照决定查询目标，不能在两个稳定窗口之间
    // 先提交角色、再以更高 revision 的会话历史覆盖同一批 UI 事实。
    const value = await options.load(before);
    const after = await options.readState();
    if (after.state_revision !== before.state_revision) continue;
    if (!options.acceptRevision(after.state_revision)) continue;
    return { value, state: after };
  }
  throw new Error('运行时状态在读取期间持续变化，请稍后重试。');
}

/**
 * 在角色、会话和运行时请求之间共享单调 revision。
 * 旧响应可以正常结束，但不得再覆盖较新的后端事实状态。
 */
export function useRuntimeStateRevision() {
  const revisionRef = useRef(0);
  const [stateRevision, setStateRevision] = useState(0);

  const acceptStateRevision = useCallback((candidate: number): boolean => {
    if (!Number.isSafeInteger(candidate) || candidate < revisionRef.current) return false;
    if (candidate > revisionRef.current) {
      revisionRef.current = candidate;
      setStateRevision(candidate);
    }
    return true;
  }, []);

  const isStateRevisionCurrent = useCallback(
    (candidate: number): boolean =>
      Number.isSafeInteger(candidate) && candidate >= revisionRef.current,
    []
  );

  const currentStateRevision = useCallback(() => revisionRef.current, []);

  return {
    stateRevision,
    acceptStateRevision,
    isStateRevisionCurrent,
    currentStateRevision
  };
}
