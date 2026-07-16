import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useFollowLatest } from './useFollowLatest';

const scrollTo = vi.fn();
const originalScrollTo = HTMLElement.prototype.scrollTo;

function StoryScrollHarness({ updates }: { updates: string[] }) {
  const { scrollRef, hasUnreadUpdate, onScroll, scrollToLatest } = useFollowLatest({
    conversationId: 'story-1',
    updates
  });

  return (
    <>
      <div data-testid="story-scroll" ref={scrollRef} onScroll={onScroll} />
      {hasUnreadUpdate && (
        <button type="button" onClick={() => scrollToLatest('smooth')}>
          有新回复，回到最新
        </button>
      )}
    </>
  );
}

function setScrollMetrics(element: HTMLElement, scrollTop: number) {
  Object.defineProperties(element, {
    scrollHeight: { configurable: true, get: () => 1000 },
    clientHeight: { configurable: true, get: () => 400 },
    scrollTop: { configurable: true, get: () => scrollTop }
  });
}

describe('useFollowLatest', () => {
  beforeEach(() => {
    Object.defineProperty(HTMLElement.prototype, 'scrollTo', {
      configurable: true,
      value: scrollTo
    });
    scrollTo.mockClear();
  });

  afterEach(() => {
    cleanup();
    Object.defineProperty(HTMLElement.prototype, 'scrollTo', {
      configurable: true,
      value: originalScrollTo
    });
  });

  it('用户翻阅旧记录时保留位置，并给出回到最新的提示', async () => {
    const view = render(<StoryScrollHarness updates={['第一条']} />);
    const storyScroll = screen.getByTestId('story-scroll');
    setScrollMetrics(storyScroll, 120);
    fireEvent.scroll(storyScroll);

    view.rerender(<StoryScrollHarness updates={['第一条', '新回复']} />);

    await waitFor(() => {
      expect(screen.getByRole('button', { name: '有新回复，回到最新' })).toBeVisible();
    });
    fireEvent.click(screen.getByRole('button', { name: '有新回复，回到最新' }));
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 1000, behavior: 'smooth' });
    expect(screen.queryByRole('button', { name: '有新回复，回到最新' })).not.toBeInTheDocument();
  });

  it('用户正在底部阅读时，新增内容自动跟随到底部', async () => {
    const view = render(<StoryScrollHarness updates={['第一条']} />);
    const storyScroll = screen.getByTestId('story-scroll');
    setScrollMetrics(storyScroll, 600);
    fireEvent.scroll(storyScroll);
    scrollTo.mockClear();

    view.rerender(<StoryScrollHarness updates={['第一条', '新回复']} />);

    await waitFor(() => {
      expect(scrollTo).toHaveBeenCalledWith({ top: 1000, behavior: 'auto' });
    });
    expect(screen.queryByRole('button', { name: '有新回复，回到最新' })).not.toBeInTheDocument();
  });
});
