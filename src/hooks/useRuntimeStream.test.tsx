import { act, cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { RuntimeEvent } from '@/types';

const api = vi.hoisted(() => ({
  answerRuntimeUserQuestion: vi.fn(),
  approveRuntimeTool: vi.fn(),
  cancelRuntimeTool: vi.fn(),
  cancelRuntimeTurn: vi.fn(),
  cancelRuntimeUserQuestion: vi.fn(),
  streamRuntimeChat: vi.fn()
}));

vi.mock('@/api', () => api);

import { useRuntimeStream } from './useRuntimeStream';

type RuntimeHook = ReturnType<typeof useRuntimeStream>;
type RuntimeOptions = Parameters<typeof useRuntimeStream>[0];

function Harness({
  current,
  options = {}
}: {
  current: { value?: RuntimeHook };
  options?: Partial<RuntimeOptions>;
}) {
  current.value = useRuntimeStream({ onDialogue: vi.fn(), ...options });
  return null;
}

describe('useRuntimeStream 交互恢复', () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  function setup(options: Partial<RuntimeOptions> = {}) {
    const current: { value?: RuntimeHook } = {};
    let onEvent: ((event: RuntimeEvent) => void) | undefined;
    api.streamRuntimeChat.mockImplementation((_request, options) => {
      onEvent = options.onEvent;
      return new Promise<void>((_resolve, reject) => {
        options.signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      });
    });
    render(<Harness current={current} options={options} />);
    return { current, emit: (event: RuntimeEvent) => onEvent?.(event) };
  }

  it('审批请求失败后保留 waiting 卡片并附带可见错误', async () => {
    api.approveRuntimeTool.mockRejectedValue(new Error('网络不可用'));
    const { current, emit } = setup();
    await act(async () => {
      await current.value?.sendMessage('执行操作');
      emit({ type: 'turn_started', turn_id: 'turn-1' });
      emit({
        type: 'approval_pending',
        approval_id: 'approval-1',
        state: 'waiting_approval'
      });
    });

    await act(async () => {
      await current.value?.resolveApproval('approval-1', true);
    });

    const step = current.value?.messages[1].process.find((item) => item.approvalId === 'approval-1');
    expect(step?.state).toBe('waiting_approval');
    expect(step?.interactionError).toContain('网络不可用');
  });

  it('成功提交取消后等待后端终态，再将本地流标记为已停止', async () => {
    api.cancelRuntimeTurn.mockResolvedValue({ status: 'ok' });
    const { current, emit } = setup();
    await act(async () => {
      await current.value?.sendMessage('开始长任务');
      emit({ type: 'turn_started', turn_id: 'turn-2' });
    });

    await act(async () => {
      await current.value?.cancelCurrentTurn();
    });

    expect(api.cancelRuntimeTurn).toHaveBeenCalledWith('turn-2');
    expect(current.value?.busy).toBe(true);
    expect(current.value?.canceling).toBe(true);

    await act(async () => {
      emit({ type: 'error', message: '当前 turn 已由用户取消。' });
    });

    expect(current.value?.busy).toBe(false);
    expect(current.value?.canceling).toBe(false);
    expect(current.value?.messages[1].status).toBe('cancelled');
  });

  it('后端终态先于取消接口响应时不会把已停止卡片改回 active', async () => {
    let resolveCancellation!: (value: { status: string }) => void;
    api.cancelRuntimeTurn.mockReturnValue(
      new Promise((resolve) => {
        resolveCancellation = resolve;
      })
    );
    const { current, emit } = setup();
    await act(async () => {
      await current.value?.sendMessage('开始长任务');
      emit({ type: 'turn_started', turn_id: 'turn-race' });
    });

    let cancelRequest!: Promise<void>;
    act(() => {
      cancelRequest = current.value!.cancelCurrentTurn();
    });
    await act(async () => {
      emit({ type: 'error', message: '当前 turn 已由用户取消。' });
    });
    await act(async () => {
      resolveCancellation({ status: '已取消' });
      await cancelRequest;
    });

    expect(current.value?.busy).toBe(false);
    expect(current.value?.messages[1].status).toBe('cancelled');
    expect(current.value?.messages[1].process.at(-1)?.state).not.toBe('active');
    expect(current.value?.runtimeStatus).toBe('当前回复已停止。');
  });

  it('在 32ms 窗口内成组提交文本、推理和工具输出 delta', async () => {
    vi.useFakeTimers();
    const { current, emit } = setup();
    await act(async () => {
      await current.value?.sendMessage('观察批处理');
      emit({ type: 'turn_started', turn_id: 'turn-batch' });
      emit({
        type: 'tool_call',
        call_id: 'call-batch',
        name: 'command_run',
        phase: 'tool_running'
      });
    });

    act(() => emit({ type: 'assistant_delta', content: '你好' }));
    act(() => emit({ type: 'assistant_delta', content: '，世界' }));
    act(() => emit({ type: 'reasoning_delta', content: '先分析' }));
    act(() =>
      emit({
        type: 'tool_output_delta',
        call_id: 'call-batch',
        stream: 'stdout',
        content: '第一段'
      })
    );

    expect(current.value?.messages[1].content).toBe('');
    expect(current.value?.messages[1].reasoning).toBe('');
    expect(current.value?.runtimeStatus).not.toContain('输出更新');

    act(() => vi.advanceTimersByTime(32));

    expect(current.value?.messages[1].content).toBe('你好，世界');
    expect(current.value?.messages[1].reasoning).toBe('先分析');
    expect(
      current.value?.messages[1].process.find((step) => step.callId === 'call-batch')?.detail
    ).toContain('[stdout] 第一段');
  });

  it('终态到达时强制刷新未到时的 delta，不丢失尾块', async () => {
    vi.useFakeTimers();
    const { current, emit } = setup();
    await act(async () => {
      await current.value?.sendMessage('检查尾块');
      emit({ type: 'turn_started', turn_id: 'turn-tail' });
    });

    act(() => {
      emit({ type: 'assistant_delta', content: '未到时尾块' });
      emit({ type: 'reasoning_delta', content: '尾部推理' });
      emit({ type: 'done' });
    });

    expect(current.value?.messages[1].content).toBe('未到时尾块');
    expect(current.value?.messages[1].reasoning).toBe('尾部推理');
    expect(current.value?.messages[1].status).toBe('completed');
    expect(current.value?.messages[1].streaming).toBe(false);
  });

  it('展示每轮实际模型与回退来源，并把冻结音色传给合成请求', async () => {
    const onTurnStarted = vi.fn();
    const onSpeech = vi.fn().mockResolvedValue(undefined);
    const onDialogue = vi.fn();
    const { current, emit } = setup({ onTurnStarted, onSpeech, onDialogue });
    await act(async () => {
      await current.value?.sendMessage('检查角色偏好');
      emit({
        type: 'turn_started',
        turn_id: 'turn-runtime-preference',
        model: 'deepseek / deepseek-v4-pro',
        model_source: 'global_active',
        model_fallback: true,
        model_fallback_reason: '角色引用失效；已使用全局活动模型。',
        active_voice_id: 'persona-voice',
        voice_source: 'persona_preference'
      });
      emit({
        type: 'speech_started',
        text: '你好',
        voice_id: 'persona-voice'
      });
      emit({ type: 'assistant_message', content: '你好' });
    });

    const snapshotStep = current.value?.messages[1].process.find(
      (step) => step.message === '运行时已创建本轮对话快照。'
    );
    expect(onTurnStarted).toHaveBeenCalledWith(
      expect.objectContaining({ model_source: 'global_active', model_fallback: true })
    );
    expect(snapshotStep?.detail).toContain('全局活动配置');
    expect(snapshotStep?.detail).toContain('模型回退');
    expect(snapshotStep?.detail).toContain('persona-voice（角色偏好）');
    expect(onSpeech).toHaveBeenCalledWith('你好', 'persona-voice');
    expect(onDialogue).toHaveBeenLastCalledWith(
      expect.objectContaining({ role: 'assistant', voiceId: 'persona-voice' })
    );
  });
});
