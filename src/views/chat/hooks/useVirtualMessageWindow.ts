import { useEffect, useState } from 'react';
import type { RefObject } from 'react';

const DEFAULT_ESTIMATED_ROW_HEIGHT = 132;
const DEFAULT_OVERSCAN = 8;
const DEFAULT_THRESHOLD = 80;

interface VirtualMessageWindow {
  start: number;
  end: number;
  paddingBefore: number;
  paddingAfter: number;
  virtualized: boolean;
}

function calculateWindow(
  itemCount: number,
  scrollTop: number,
  viewportHeight: number,
  estimatedRowHeight: number,
  overscan: number,
  threshold: number
): VirtualMessageWindow {
  if (itemCount <= threshold) {
    return { start: 0, end: itemCount, paddingBefore: 0, paddingAfter: 0, virtualized: false };
  }
  const firstVisible = Math.floor(Math.max(0, scrollTop) / estimatedRowHeight);
  const visibleCount = Math.ceil(Math.max(viewportHeight, estimatedRowHeight) / estimatedRowHeight);
  const start = Math.max(0, firstVisible - overscan);
  const end = Math.min(itemCount, firstVisible + visibleCount + overscan);
  return {
    start,
    end,
    paddingBefore: start * estimatedRowHeight,
    paddingAfter: (itemCount - end) * estimatedRowHeight,
    virtualized: true
  };
}

export function useVirtualMessageWindow(
  itemCount: number,
  scrollContainerRef?: RefObject<HTMLElement | null>,
  options: {
    estimatedRowHeight?: number;
    overscan?: number;
    threshold?: number;
  } = {}
): VirtualMessageWindow {
  const estimatedRowHeight = options.estimatedRowHeight ?? DEFAULT_ESTIMATED_ROW_HEIGHT;
  const overscan = options.overscan ?? DEFAULT_OVERSCAN;
  const threshold = options.threshold ?? DEFAULT_THRESHOLD;
  const [windowState, setWindowState] = useState(() =>
    calculateWindow(itemCount, 0, 720, estimatedRowHeight, overscan, threshold)
  );

  useEffect(() => {
    const element = scrollContainerRef?.current;
    const update = () => {
      const viewportHeight = element?.clientHeight || 720;
      const scrollTop = element?.scrollTop || 0;
      setWindowState(
        calculateWindow(
          itemCount,
          scrollTop,
          viewportHeight,
          estimatedRowHeight,
          overscan,
          threshold
        )
      );
    };

    update();
    element?.addEventListener('scroll', update, { passive: true });
    const observer =
      element && typeof ResizeObserver !== 'undefined' ? new ResizeObserver(update) : null;
    if (element && observer) observer.observe(element);
    window.addEventListener('resize', update);
    return () => {
      element?.removeEventListener('scroll', update);
      observer?.disconnect();
      window.removeEventListener('resize', update);
    };
  }, [estimatedRowHeight, itemCount, overscan, scrollContainerRef, threshold]);

  return windowState;
}
