import { useCallback, useMemo, useState } from 'react';

import {
  derivePersonaPresence,
  resourceHasData,
  type ResourceState,
  type StoryPersonaSnapshot
} from '@/views/story/types';

function formatResourceError(error: unknown): string {
  return error instanceof Error ? error.message : '无法读取角色状态。';
}

/**
 * 剧情首页只维护加载生命周期，不接管角色编辑或聊天请求。
 * refreshing/stale 会保留最后一次可信快照，避免切换角色时混合新旧内容。
 */
export function useStoryWorkspaceState() {
  const [resourceState, setResourceState] = useState<ResourceState<StoryPersonaSnapshot>>({
    status: 'initial'
  });

  const beginLoad = useCallback(() => {
    setResourceState((current) =>
      resourceHasData(current)
        ? { status: 'refreshing', data: current.data }
        : { status: 'loading' }
    );
  }, []);

  const commitSnapshot = useCallback((snapshot: StoryPersonaSnapshot) => {
    setResourceState({ status: 'ready', data: snapshot });
  }, []);

  const failLoad = useCallback((error: unknown) => {
    const message = formatResourceError(error);
    setResourceState((current) =>
      resourceHasData(current)
        ? { status: 'stale', data: current.data, error: message }
        : { status: 'failed', error: message }
    );
  }, []);

  const presence = useMemo(() => derivePersonaPresence(resourceState), [resourceState]);
  const mutationBlockReason = useMemo(() => {
    if (resourceState.status === 'refreshing') {
      return '角色状态正在同步，请等待完成后再操作。';
    }
    if (resourceState.status === 'stale') {
      return '角色状态已过期，请先重试同步。';
    }
    if (resourceState.status === 'initial' || resourceState.status === 'loading') {
      return '角色状态尚未加载完成。';
    }
    if (resourceState.status === 'failed') {
      return '角色状态读取失败，请先重试。';
    }
    return null;
  }, [resourceState.status]);
  const chatMutationBlockReason = useMemo(() => {
    if (mutationBlockReason) return mutationBlockReason;
    if (presence?.kind !== 'active_persona') {
      return '请先选择并启用一个角色。';
    }
    return null;
  }, [mutationBlockReason, presence]);

  return {
    resourceState,
    presence,
    mutationBlockReason,
    chatMutationBlockReason,
    beginLoad,
    commitSnapshot,
    failLoad
  };
}
