import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  fetchVoiceCapabilities: vi.fn(),
  synthesizeSpeech: vi.fn(),
  transcribeAudio: vi.fn()
}));

vi.mock('@/api', () => api);

import { useVoiceRuntime } from './useVoiceRuntime';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function transcriptContext() {
  return { inputRevision: 3, stateRevision: 7, personaId: 'persona-a' };
}

function installRecordingEnvironment() {
  const stopTrack = vi.fn();
  const getUserMedia = vi.fn(async () => ({
    getTracks: () => [{ stop: stopTrack }]
  }) as unknown as MediaStream);
  vi.stubGlobal('navigator', {
    ...navigator,
    mediaDevices: { getUserMedia }
  });

  const source = { connect: vi.fn(), disconnect: vi.fn() };
  const processor = {
    onaudioprocess: null as ScriptProcessorNode['onaudioprocess'],
    connect: vi.fn(),
    disconnect: vi.fn()
  };
  const silentOutput = {
    gain: { value: 1 },
    connect: vi.fn(),
    disconnect: vi.fn()
  };
  const close = vi.fn(() => Promise.resolve());
  const audioContext = {
    sampleRate: 48_000,
    destination: {},
    createMediaStreamSource: vi.fn(() => source),
    createScriptProcessor: vi.fn(() => processor),
    createGain: vi.fn(() => silentOutput),
    close
  };
  function AudioContextMock() {
    return audioContext;
  }
  vi.stubGlobal('AudioContext', AudioContextMock);

  return { audioContext, close, getUserMedia, processor, silentOutput, source, stopTrack };
}

describe('useVoiceRuntime', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.fetchVoiceCapabilities.mockResolvedValue({ tts: true, speech_recognition: true });
    api.synthesizeSpeech.mockResolvedValue(new Blob(['audio'], { type: 'audio/mpeg' }));
    api.transcribeAudio.mockResolvedValue('');
    vi.stubGlobal('URL', {
      ...URL,
      createObjectURL: vi.fn(() => 'blob:muse-test'),
      revokeObjectURL: vi.fn()
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('麦克风权限请求未完成时重复点击只创建一个录音流', async () => {
    const permission = deferred<MediaStream>();
    const stop = vi.fn();
    const getUserMedia = vi.fn(() => permission.promise);
    vi.stubGlobal('navigator', {
      ...navigator,
      mediaDevices: { getUserMedia }
    });

    const processor = {
      onaudioprocess: null,
      connect: vi.fn(),
      disconnect: vi.fn()
    };
    const audioContext = {
      sampleRate: 48_000,
      destination: {},
      createMediaStreamSource: vi.fn(() => ({ connect: vi.fn(), disconnect: vi.fn() })),
      createScriptProcessor: vi.fn(() => processor),
      createGain: vi.fn(() => ({ gain: { value: 1 }, connect: vi.fn(), disconnect: vi.fn() })),
      close: vi.fn(() => Promise.resolve())
    };
    function AudioContextMock() {
      return audioContext;
    }
    vi.stubGlobal('AudioContext', AudioContextMock);

    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript: vi.fn(), getTranscriptContext: transcriptContext })
    );
    await waitFor(() => expect(result.current.voice.speechRecognitionAvailable).toBe(true));

    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = result.current.startVoiceInput();
      second = result.current.startVoiceInput();
    });
    expect(getUserMedia).toHaveBeenCalledTimes(1);

    permission.resolve({ getTracks: () => [{ stop }] } as unknown as MediaStream);
    await act(async () => {
      await Promise.all([first, second]);
    });
    expect(result.current.voice.phase).toBe('recording');
    expect(result.current.voice.listening).toBe(true);
  });

  it('停止语音会使尚未完成的麦克风权限请求失效', async () => {
    const permission = deferred<MediaStream>();
    const stopTrack = vi.fn();
    const getUserMedia = vi.fn(() => permission.promise);
    vi.stubGlobal('navigator', {
      ...navigator,
      mediaDevices: { getUserMedia }
    });
    const createMediaStreamSource = vi.fn();
    function AudioContextMock() {
      return { createMediaStreamSource };
    }
    vi.stubGlobal('AudioContext', AudioContextMock);

    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript: vi.fn(), getTranscriptContext: transcriptContext })
    );
    await waitFor(() => expect(result.current.voice.speechRecognitionAvailable).toBe(true));

    let recordingStarted!: Promise<void>;
    act(() => {
      recordingStarted = result.current.startVoiceInput();
    });
    expect(result.current.voice.phase).toBe('requesting_permission');

    act(() => result.current.stopVoice());
    expect(result.current.voice.phase).toBe('idle');

    permission.resolve({ getTracks: () => [{ stop: stopTrack }] } as unknown as MediaStream);
    await act(async () => {
      await recordingStarted;
    });
    expect(stopTrack).toHaveBeenCalledTimes(1);
    expect(createMediaStreamSource).not.toHaveBeenCalled();
    expect(result.current.voice.phase).toBe('idle');
    expect(result.current.voice.listening).toBe(false);
  });

  it('停止语音会立即释放录音资源并丢弃录音内容', async () => {
    const runtime = installRecordingEnvironment();
    const onTranscript = vi.fn();
    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript, getTranscriptContext: transcriptContext })
    );
    await waitFor(() => expect(result.current.voice.speechRecognitionAvailable).toBe(true));

    await act(async () => {
      await result.current.startVoiceInput();
    });
    expect(result.current.voice.phase).toBe('recording');

    act(() => result.current.stopVoice());

    expect(runtime.stopTrack).toHaveBeenCalledTimes(1);
    expect(runtime.processor.disconnect).toHaveBeenCalledTimes(1);
    expect(runtime.source.disconnect).toHaveBeenCalledTimes(1);
    expect(runtime.silentOutput.disconnect).toHaveBeenCalledTimes(1);
    expect(runtime.close).toHaveBeenCalledTimes(1);
    expect(api.transcribeAudio).not.toHaveBeenCalled();
    expect(onTranscript).not.toHaveBeenCalled();
    expect(result.current.voice.phase).toBe('idle');
    expect(result.current.voice.listening).toBe(false);
  });

  it('停止语音会中止在途转写并阻止迟到结果写回旧上下文', async () => {
    const runtime = installRecordingEnvironment();
    const transcription = deferred<string>();
    api.transcribeAudio.mockReturnValue(transcription.promise);
    const onTranscript = vi.fn();
    const capturedContext = transcriptContext();
    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript, getTranscriptContext: () => capturedContext })
    );
    await waitFor(() => expect(result.current.voice.speechRecognitionAvailable).toBe(true));

    await act(async () => {
      await result.current.startVoiceInput();
    });
    act(() => {
      runtime.processor.onaudioprocess?.call(
        runtime.processor as unknown as ScriptProcessorNode,
        {
          inputBuffer: { getChannelData: () => new Float32Array([0.25, -0.25]) }
        } as unknown as AudioProcessingEvent
      );
    });

    let transcriptionFinished!: Promise<void>;
    await act(async () => {
      transcriptionFinished = result.current.startVoiceInput();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(api.transcribeAudio).toHaveBeenCalledTimes(1);
    const signal = api.transcribeAudio.mock.calls[0]?.[2] as AbortSignal;
    expect(signal.aborted).toBe(false);

    act(() => result.current.stopVoice());
    expect(signal.aborted).toBe(true);

    transcription.resolve('不应写回的新文本');
    await act(async () => {
      await transcriptionFinished;
    });
    expect(onTranscript).not.toHaveBeenCalled();
    expect(result.current.voice.phase).toBe('idle');
  });

  it('成功转写会原样带回录音开始时捕获的上下文快照', async () => {
    const runtime = installRecordingEnvironment();
    api.transcribeAudio.mockResolvedValue('识别结果');
    const onTranscript = vi.fn();
    let currentContext = transcriptContext();
    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript, getTranscriptContext: () => currentContext })
    );
    await waitFor(() => expect(result.current.voice.speechRecognitionAvailable).toBe(true));

    await act(async () => {
      await result.current.startVoiceInput();
    });
    runtime.processor.onaudioprocess?.call(
      runtime.processor as unknown as ScriptProcessorNode,
      {
        inputBuffer: { getChannelData: () => new Float32Array([0.5]) }
      } as unknown as AudioProcessingEvent
    );
    currentContext = { inputRevision: 4, stateRevision: 8, personaId: 'persona-b' };

    await act(async () => {
      await result.current.startVoiceInput();
    });

    expect(onTranscript).toHaveBeenCalledWith('识别结果', transcriptContext());
  });

  it('停止播放会结算 waitUntilEnded，避免调用方永久悬挂', async () => {
    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript: vi.fn(), getTranscriptContext: transcriptContext })
    );
    await waitFor(() => expect(result.current.voice.ttsAvailable).toBe(true));

    const audio = document.createElement('audio');
    vi.spyOn(audio, 'play').mockResolvedValue();
    vi.spyOn(audio, 'pause').mockImplementation(() => undefined);
    vi.spyOn(audio, 'load').mockImplementation(() => undefined);
    result.current.audioRef.current = audio;

    let playback!: Promise<string | null>;
    await act(async () => {
      playback = result.current.speak('测试语音', {
        forceEnabled: true,
        waitUntilEnded: true
      });
      await Promise.resolve();
      await Promise.resolve();
    });
    act(() => result.current.stopVoice());

    await expect(playback).resolves.toBe('语音播放已停止。');
    expect(result.current.voice.phase).toBe('idle');
  });

  it('手动开启角色播报时沿用冻结的角色音色', async () => {
    const { result } = renderHook(() =>
      useVoiceRuntime({ onTranscript: vi.fn(), getTranscriptContext: transcriptContext })
    );
    await waitFor(() => expect(result.current.voice.ttsAvailable).toBe(true));

    const audio = document.createElement('audio');
    vi.spyOn(audio, 'play').mockResolvedValue();
    vi.spyOn(audio, 'pause').mockImplementation(() => undefined);
    vi.spyOn(audio, 'load').mockImplementation(() => undefined);
    result.current.audioRef.current = audio;

    act(() => {
      result.current.toggleVoice({
        role: 'assistant',
        text: '使用角色音色播报',
        voiceId: 'persona-voice'
      });
    });

    await waitFor(() =>
      expect(api.synthesizeSpeech).toHaveBeenCalledWith(
        '使用角色音色播报',
        expect.any(AbortSignal),
        'persona-voice'
      )
    );
  });
});
