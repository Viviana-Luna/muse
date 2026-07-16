import { useCallback, useLayoutEffect, useRef, useState } from 'react';
import type { UIEventHandler } from 'react';

const BOTTOM_TOLERANCE = 40;

function isNearBottom(element: HTMLElement) {
  return element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_TOLERANCE;
}

interface UseFollowLatestOptions {
  /** 当前会话切换时强制定位到其最新一条记录。 */
  conversationId: string;
  /** 每次消息或流式内容变化都会产生一个新引用。 */
  updates: readonly unknown[];
}

/**
 * 让剧情阅读区只在用户正阅读底部时自动跟随。
 * 用户主动翻阅旧消息后，新增内容不会抢走阅读位置，而是由显式按钮恢复跟随。
 */
export function useFollowLatest({ conversationId, updates }: UseFollowLatestOptions) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followingLatestRef = useRef(true);
  const forceFollowRef = useRef(true);
  const [hasUnreadUpdate, setHasUnreadUpdate] = useState(false);

  const scrollToLatest = useCallback((behavior: ScrollBehavior = 'smooth') => {
    const element = scrollRef.current;
    followingLatestRef.current = true;
    forceFollowRef.current = false;
    setHasUnreadUpdate(false);
    if (!element) return;
    element.scrollTo({ top: element.scrollHeight, behavior });
  }, []);

  const onScroll = useCallback<UIEventHandler<HTMLDivElement>>((event) => {
    const followsLatest = isNearBottom(event.currentTarget);
    followingLatestRef.current = followsLatest;
    if (followsLatest) setHasUnreadUpdate(false);
  }, []);

  useLayoutEffect(() => {
    // 切换剧情时应从该剧情的最新位置开始，而不是继承上一段的阅读位置。
    followingLatestRef.current = true;
    forceFollowRef.current = true;
    setHasUnreadUpdate(false);
  }, [conversationId]);

  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (!element) return;

    if (forceFollowRef.current || followingLatestRef.current) {
      element.scrollTo({ top: element.scrollHeight, behavior: 'auto' });
      followingLatestRef.current = true;
      forceFollowRef.current = false;
      setHasUnreadUpdate(false);
      return;
    }

    setHasUnreadUpdate(true);
  }, [updates, conversationId]);

  const followOutgoingMessage = useCallback(() => {
    // 发送是明确的“继续当前剧情”意图，必须立即恢复跟随状态。
    scrollToLatest('auto');
  }, [scrollToLatest]);

  return {
    scrollRef,
    hasUnreadUpdate,
    onScroll,
    scrollToLatest,
    followOutgoingMessage
  };
}
