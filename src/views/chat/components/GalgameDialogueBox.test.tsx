import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useRef } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

import { GalgameDialogueBox } from './GalgameDialogueBox';
import type { ChatMessage } from '@/hooks/useRuntimeStream';

function buildMessages(count: number): ChatMessage[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index}`,
    role: index % 2 ? 'assistant' : 'user',
    content: `第 ${index} 条消息`,
    status: 'completed',
    process: []
  }));
}

function VirtualizedHarness({ messages }: { messages: ChatMessage[] }) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  return (
    <div ref={scrollRef} style={{ height: 600, overflow: 'auto' }}>
      <GalgameDialogueBox messages={messages} scrollContainerRef={scrollRef} />
    </div>
  );
}

describe('GalgameDialogueBox', () => {
  beforeAll(async () => {
    // 先解析消息渲染 chunk，避免全量并发测试结束时仍有 React.lazy 调度落到已销毁的 jsdom。
    await import('./MarkdownMessage');
  });

  afterEach(() => cleanup());

  it('在空会话中展示开场建议，并把选择交给输入区', () => {
    const onUsePrompt = vi.fn();
    render(
      <GalgameDialogueBox
        messages={[]}
        activePersonaName="雨灵"
        activePersonaScenario="雨夜便利店"
        emptyText="先和我打个招呼吧。"
        onUsePrompt={onUsePrompt}
      />
    );

    expect(screen.getByRole('heading', { name: '雨灵正在这里' })).toBeVisible();
    const prompt = screen.getByRole('button', { name: '从“雨夜便利店”开始' });
    fireEvent.click(prompt);
    expect(onUsePrompt).toHaveBeenCalledWith('从“雨夜便利店”开始');
  });

  it('1000 条历史只挂载当前窗口，并保留未变化消息节点', async () => {
    const messages = buildMessages(1000);
    const { container, rerender } = render(<VirtualizedHarness messages={messages} />);
    const scrollContainer = container.firstElementChild as HTMLDivElement;
    Object.defineProperty(scrollContainer, 'clientHeight', { configurable: true, value: 600 });
    scrollContainer.scrollTop = 132_000;
    fireEvent.scroll(scrollContainer);

    await waitFor(() =>
      expect(document.querySelector('[data-message-id="message-998"]')).not.toBeNull()
    );
    const rendered = screen.getAllByTestId('story-message');
    expect(rendered.length).toBeLessThan(40);
    const stableNode = document.querySelector('[data-message-id="message-998"]');
    expect(stableNode).not.toBeNull();

    const nextMessages = [...messages];
    nextMessages[999] = { ...nextMessages[999], content: '尾消息流式更新' };
    rerender(<VirtualizedHarness messages={nextMessages} />);

    expect(document.querySelector('[data-message-id="message-998"]')).toBe(stableNode);
    expect(screen.getByText('尾消息流式更新')).toBeVisible();
    expect(screen.getAllByTestId('story-message').length).toBeLessThan(40);
  });

  it('为聊天富文本挂载受约束的 Markdown 容器', async () => {
    render(<GalgameDialogueBox messages={buildMessages(1)} />);

    await waitFor(() => expect(screen.getByText('第 0 条消息')).toBeVisible());
    expect(screen.getByText('第 0 条消息').closest('.message-markdown')).not.toBeNull();
  });
});
