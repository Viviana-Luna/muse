// 语音运行时 Hook，负责麦克风录音、后端转写、语音合成播放和频谱可视化。

import { useCallback, useEffect, useRef, useState } from 'react';

import { fetchVoiceCapabilities, synthesizeSpeech, transcribeAudio } from '@/api';
import type { AppToastInput } from '@/hooks/useAppToast';

type VoiceDialogueRole = 'system' | 'user' | 'assistant';

interface VoiceDialogueSnapshot {
  role: VoiceDialogueRole;
  text: string;
  voiceId?: string;
}

// 录音开始时捕获的写入上下文，用于拒绝跨角色或跨状态版本的迟到转写。
export interface VoiceTranscriptContext {
  inputRevision: number;
  stateRevision: number;
  personaId: string | null;
}

// 语音运行时状态与控制方法。
export interface VoiceRuntime {
  /// 后端语音合成是否可用（`tts` 段已启用）。
  ttsAvailable: boolean;
  /// 语音识别服务是否可用。
  speechRecognitionAvailable: boolean;
  /// 用户是否开启语音播报。
  voiceEnabled: boolean;
  /// 是否正在录音采集（语音输入中）。
  listening: boolean;
  /// 当前语音阶段，用于阻止录音、转写与播放的竞态。
  phase: VoiceRuntimePhase;
  /// 状态文案，展示给用户。
  status: string;
}

export type VoiceRuntimePhase =
  | 'idle'
  | 'requesting_permission'
  | 'recording'
  | 'transcribing'
  | 'synthesizing'
  | 'playing'
  | 'stopping'
  | 'error';

interface SpeechRequestOptions {
  forceEnabled?: boolean;
  voiceId?: string;
  reportRuntimeStatus?: boolean;
  waitUntilEnded?: boolean;
}

interface PcmRecorderRuntime {
  context: AudioContext;
  source: MediaStreamAudioSourceNode;
  processor: ScriptProcessorNode;
  silentOutput: GainNode;
  chunks: Float32Array[];
}

interface UseVoiceRuntimeOptions {
  onTranscript: (text: string, context: VoiceTranscriptContext) => void;
  getTranscriptContext: () => VoiceTranscriptContext;
  onNotify?: (notification: AppToastInput) => void;
}

interface PendingPlayback {
  requestId: number;
  url: string;
  resolve: (message: string | null) => void;
  settled: boolean;
}

function voiceIdleStatus(voiceEnabled: boolean) {
  return voiceEnabled ? '语音已开启' : '语音未开启';
}

function formatVoiceError(error: unknown, fallback: string) {
  return error instanceof Error ? error.message : fallback;
}

function microphoneAccessError(error: unknown) {
  const name = error instanceof DOMException || error instanceof Error ? error.name : '';
  switch (name) {
    case 'NotAllowedError':
    case 'SecurityError':
      return {
        status: '麦克风权限被拒绝',
        description: '请在系统设置的麦克风隐私权限中允许 Muse 访问，然后返回应用重试。'
      };
    case 'NotFoundError':
      return {
        status: '未检测到可用麦克风',
        description: '请连接或启用麦克风设备，然后重试语音输入。'
      };
    case 'NotReadableError':
    case 'AbortError':
      return {
        status: '麦克风暂时无法使用',
        description: '麦克风可能正被其他应用占用，请关闭占用后重试。'
      };
    default:
      return {
        status: '无法访问麦克风',
        description: '当前环境无法访问麦克风，请检查系统权限或音频设备后重试。'
      };
  }
}

function mergePcmChunks(chunks: Float32Array[]): Float32Array {
  const length = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const merged = new Float32Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk, offset);
    offset += chunk.length;
  }
  return merged;
}

function resamplePcm(samples: Float32Array, sourceRate: number, targetRate = 16_000): Float32Array {
  if (sourceRate === targetRate) return samples;
  const outputLength = Math.max(1, Math.round((samples.length * targetRate) / sourceRate));
  const output = new Float32Array(outputLength);
  const ratio = sourceRate / targetRate;
  for (let index = 0; index < outputLength; index += 1) {
    const position = index * ratio;
    const left = Math.floor(position);
    const right = Math.min(left + 1, samples.length - 1);
    const fraction = position - left;
    output[index] = samples[left] * (1 - fraction) + samples[right] * fraction;
  }
  return output;
}

function encodePcm16Wav(samples: Float32Array, sampleRate = 16_000): Blob {
  const buffer = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(buffer);
  const writeText = (offset: number, value: string) => {
    for (let index = 0; index < value.length; index += 1) {
      view.setUint8(offset + index, value.charCodeAt(index));
    }
  };
  writeText(0, 'RIFF');
  view.setUint32(4, 36 + samples.length * 2, true);
  writeText(8, 'WAVE');
  writeText(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeText(36, 'data');
  view.setUint32(40, samples.length * 2, true);
  samples.forEach((sample, index) => {
    const clamped = Math.max(-1, Math.min(1, sample));
    view.setInt16(44 + index * 2, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
  });
  return new Blob([buffer], { type: 'audio/wav' });
}

function fillRoundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number
) {
  const safeRadius = Math.min(radius, width / 2, height / 2);
  ctx.beginPath();
  ctx.moveTo(x + safeRadius, y);
  ctx.lineTo(x + width - safeRadius, y);
  ctx.quadraticCurveTo(x + width, y, x + width, y + safeRadius);
  ctx.lineTo(x + width, y + height - safeRadius);
  ctx.quadraticCurveTo(x + width, y + height, x + width - safeRadius, y + height);
  ctx.lineTo(x + safeRadius, y + height);
  ctx.quadraticCurveTo(x, y + height, x, y + height - safeRadius);
  ctx.lineTo(x, y + safeRadius);
  ctx.quadraticCurveTo(x, y, x + safeRadius, y);
  ctx.closePath();
  ctx.fill();
}

// 创建语音运行时 Hook，封装语音播报、语音输入和频谱画布控制。
export function useVoiceRuntime({
  onTranscript,
  getTranscriptContext,
  onNotify
}: UseVoiceRuntimeOptions) {
  const [voice, setVoice] = useState<VoiceRuntime>({
    ttsAvailable: false,
    speechRecognitionAvailable: false,
    voiceEnabled: false,
    listening: false,
    phase: 'idle',
    status: '语音未开启'
  });

  // 隐藏的音频播放器，承接后端语音合成生成的 MP3。
  const audioRef = useRef<HTMLAudioElement | null>(null);
  // 录音状态：直接采集 PCM，避免后端依赖 ffmpeg 解码浏览器 WebM。
  const pcmRecorderRef = useRef<PcmRecorderRuntime | null>(null);
  const mediaStreamRef = useRef<MediaStream | null>(null);
  // 语音合成频谱示波器：用网页音频接口读取角色语音播报的实时频域数据。
  const audioCtxRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const mediaElSourceRef = useRef<MediaElementAudioSourceNode | null>(null);
  const visualizerCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const rafRef = useRef<number | null>(null);
  const audioObjectUrlRef = useRef<string | null>(null);
  const ttsRequestIdRef = useRef(0);
  const ttsAbortRef = useRef<AbortController | null>(null);
  const pendingPlaybackRef = useRef<PendingPlayback | null>(null);
  const voicePhaseRef = useRef<VoiceRuntimePhase>('idle');
  const recordingRequestIdRef = useRef(0);
  const transcriptionRequestIdRef = useRef(0);
  const transcriptionAbortRef = useRef<AbortController | null>(null);
  const transcriptionContextRef = useRef<VoiceTranscriptContext | null>(null);
  const mountedRef = useRef(true);
  const notifyRef = useRef(onNotify);
  const transcriptRef = useRef(onTranscript);
  const getTranscriptContextRef = useRef(getTranscriptContext);

  // 同步摘除录音节点和媒体流。调用方决定是否等待 AudioContext 关闭完成。
  const detachPcmRecording = useCallback(() => {
    const runtime = pcmRecorderRef.current;
    pcmRecorderRef.current = null;
    if (runtime) {
      runtime.processor.onaudioprocess = null;
      runtime.processor.disconnect();
      runtime.source.disconnect();
      runtime.silentOutput.disconnect();
    }
    const stream = mediaStreamRef.current;
    mediaStreamRef.current = null;
    stream?.getTracks().forEach((track) => track.stop());
    return runtime;
  }, []);

  useEffect(() => {
    notifyRef.current = onNotify;
  }, [onNotify]);

  useEffect(() => {
    transcriptRef.current = onTranscript;
    getTranscriptContextRef.current = getTranscriptContext;
  }, [getTranscriptContext, onTranscript]);

  const setVoicePhase = useCallback(
    (phase: VoiceRuntimePhase, patch: Partial<VoiceRuntime> = {}) => {
      voicePhaseRef.current = phase;
      if (!mountedRef.current) return;
      setVoice((value) => ({
        ...value,
        ...(phase === 'idle' && patch.status === undefined
          ? { status: voiceIdleStatus(value.voiceEnabled) }
          : {}),
        ...patch,
        phase
      }));
    },
    []
  );

  const notifyVoiceError = useCallback((title: string, description: string) => {
    notifyRef.current?.({
      title,
      description,
      tone: 'error',
      duration: 7200
    });
  }, []);

  const refreshVoiceCapabilities = useCallback(async () => {
    try {
      const caps = await fetchVoiceCapabilities();
      setVoice((value) => ({
        ...value,
        ttsAvailable: caps.tts,
        speechRecognitionAvailable: caps.speech_recognition
      }));
    } catch {
      // 能力探测失败时保持当前 UI 状态，不打断用户编辑。
    }
  }, []);

  useEffect(() => {
    void refreshVoiceCapabilities();
  }, [refreshVoiceCapabilities]);

  /// 组件卸载时释放可能残留的麦克风资源，避免录音指示灯常亮。
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      recordingRequestIdRef.current += 1;
      transcriptionRequestIdRef.current += 1;
      ttsRequestIdRef.current += 1;
      transcriptionAbortRef.current?.abort();
      ttsAbortRef.current?.abort();
      const pending = pendingPlaybackRef.current;
      if (pending && !pending.settled) {
        pending.settled = true;
        pending.resolve('语音播放已停止。');
      }
      pendingPlaybackRef.current = null;
      const audio = audioRef.current;
      audio?.pause();
      if (audio) {
        audio.onplay = null;
        audio.onended = null;
        audio.onerror = null;
        audio.removeAttribute('src');
      }
      const recorder = detachPcmRecording();
      void recorder?.context.close();
      if (audioObjectUrlRef.current) {
        URL.revokeObjectURL(audioObjectUrlRef.current);
        audioObjectUrlRef.current = null;
      }
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
      void audioCtxRef.current?.close();
    };
  }, [detachPcmRecording]);

  /// 懒初始化语音合成频谱分析的 AudioContext 节点图。
  /// 关键：createMediaElementSource 接管 <audio> 输出后，必须 analyser.connect(destination)
  /// 才能听到声音；同一 audio 元素只能 create 一次，故用 ref 哨兵防重复。
  const ensureAnalyserGraph = useCallback(() => {
    if (analyserRef.current) return analyserRef.current;
    const audio = audioRef.current;
    const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!audio || !Ctx) return null;

    const ctx = new Ctx();
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 256;
    analyser.smoothingTimeConstant = 0.78;
    const source = ctx.createMediaElementSource(audio);
    source.connect(analyser);
    analyser.connect(ctx.destination);

    audioCtxRef.current = ctx;
    analyserRef.current = analyser;
    mediaElSourceRef.current = source;
    return analyser;
  }, []);

  /// 在用户点击开启语音时提前恢复 AudioContext，避开浏览器自动播放策略。
  const resumeAnalyserContext = useCallback(async () => {
    ensureAnalyserGraph();
    const ctx = audioCtxRef.current;
    if (ctx?.state === 'suspended') {
      await ctx.resume();
    }
  }, [ensureAnalyserGraph]);

  /// 清理上一次语音合成播放生成的 Blob URL，避免多轮播报后内存泄漏。
  const revokeCurrentAudioUrl = useCallback(() => {
    if (!audioObjectUrlRef.current) return;
    URL.revokeObjectURL(audioObjectUrlRef.current);
    audioObjectUrlRef.current = null;
  }, []);

  /// 语音可视化绘制循环：静止态显示低幅呼吸音量条，角色播报时按音频能量跳动。
  useEffect(() => {
    const canvas = visualizerCanvasRef.current;
    const analyser = analyserRef.current;
    if (!canvas) return;

    const ctx2d = canvas.getContext('2d');
    if (!ctx2d) return;

    const themeColor = (
      getComputedStyle(canvas).getPropertyValue('--theme-color').trim() || '#d8596f'
    );
    const active = voice.status === '正在语音播报' && !!analyser;
    const waveData = active ? new Uint8Array(analyser!.fftSize) : null;

    const draw = () => {
      const w = canvas.width;
      const h = canvas.height;
      ctx2d.clearRect(0, 0, w, h);
      ctx2d.globalAlpha = active ? 0.95 : 0.82;
      const barCount = 28;
      const gap = w * 0.014;
      const barWidth = Math.max(3, (w - gap * (barCount - 1)) / barCount * 0.58);
      const totalBarWidth = barWidth * barCount + gap * (barCount - 1);
      const startX = (w - totalBarWidth) / 2;
      const centerY = h * 0.5;
      ctx2d.fillStyle = themeColor;

      for (let i = 0; i < barCount; i++) {
        const progress = i / (barCount - 1);
        const envelope = 0.28 + 0.72 * Math.pow(Math.sin(progress * Math.PI), 1.12);
        let level = 0.18 + 0.44 * Math.sin(progress * Math.PI * 4.2 + 0.55) ** 2;

        if (waveData && analyser) {
          if (i === 0) {
            analyser.getByteTimeDomainData(waveData);
          }
          const start = Math.floor((i / barCount) * waveData.length);
          const end = Math.floor(((i + 1) / barCount) * waveData.length);
          let energy = 0;
          for (let j = start; j < end; j++) {
            energy += Math.abs((waveData[j] - 128) / 128);
          }
          level = Math.min(1, 0.14 + (energy / Math.max(1, end - start)) * 2.4);
        }

        const x = startX + i * (barWidth + gap);
        const barHeight = Math.max(6, h * envelope * level * (active ? 0.82 : 0.56));
        ctx2d.globalAlpha = active ? 0.92 : 0.58 + envelope * 0.16;
        fillRoundRect(ctx2d, x, centerY - barHeight / 2, barWidth, barHeight, barWidth / 2);
      }

      ctx2d.globalAlpha = 1;
      if (active) {
        rafRef.current = requestAnimationFrame(draw);
      }
    };

    draw();
    return () => {
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    };
  }, [voice.status]);

  /// 用后端语音合成文本并播放。
  /// 通过隐藏的 <audio> 元素承接音频；播报中再次调用会取消上一次请求。
  const speak = useCallback(
    async (text: string, options: SpeechRequestOptions = {}): Promise<string | null> => {
      const {
        forceEnabled = false,
        voiceId,
        reportRuntimeStatus = true,
        waitUntilEnded = false
      } = options;
      const reportStatus = (status: string) => {
        if (reportRuntimeStatus) {
          setVoice((value) => ({ ...value, status }));
        }
      };
      if (!voice.ttsAvailable) return '本地 TTS 尚未启用。';
      if (!forceEnabled && !voice.voiceEnabled) return '语音播报尚未开启。';
      if (!text) return '没有可供播报的文本。';
      const audio = audioRef.current;
      if (!audio) return '音频播放器尚未就绪。';
      const speechText = text.trim();
      if (!speechText) return '没有可供播报的文本。';
      audio.muted = false;
      audio.volume = 1;
      audio.setAttribute('playsinline', 'true');
      try {
        await resumeAnalyserContext();
      } catch {
        // 部分浏览器只允许在用户手势中恢复 AudioContext；这里提前尝试，后续播放前再兜底一次。
      }
      const requestId = ttsRequestIdRef.current + 1;
      ttsRequestIdRef.current = requestId;
      const previousPlayback = pendingPlaybackRef.current;
      if (previousPlayback && !previousPlayback.settled) {
        previousPlayback.settled = true;
        previousPlayback.resolve('试听已被新的语音请求替换。');
      }
      pendingPlaybackRef.current = null;
      ttsAbortRef.current?.abort();
      const controller = new AbortController();
      ttsAbortRef.current = controller;

      reportStatus('正在合成语音');
      setVoicePhase('synthesizing');
      audio.pause();
      audio.currentTime = 0;
      revokeCurrentAudioUrl();
      let blob: Blob;
      try {
        blob = await synthesizeSpeech(speechText, controller.signal, voiceId);
      } catch (err) {
        if (controller.signal.aborted || ttsRequestIdRef.current !== requestId) {
          return '试听已被新的语音请求替换。';
        }
        const message = formatVoiceError(err, '语音合成失败');
        reportStatus('语音合成失败');
        setVoicePhase('error');
        if (reportRuntimeStatus) notifyVoiceError('语音播报失败', message);
        return message;
      }
      if (controller.signal.aborted || ttsRequestIdRef.current !== requestId) {
        return '试听已被新的语音请求替换。';
      }
      if (ttsAbortRef.current === controller) ttsAbortRef.current = null;

      const url = URL.createObjectURL(blob);
      audioObjectUrlRef.current = url;
      audio.src = url;
      const playbackFinished = new Promise<string | null>((resolve) => {
        const pending: PendingPlayback = { requestId, url, resolve, settled: false };
        pendingPlaybackRef.current = pending;
        const settle = (message: string | null) => {
          if (pending.settled) return;
          pending.settled = true;
          if (pendingPlaybackRef.current === pending) pendingPlaybackRef.current = null;
          resolve(message);
        };
        audio.onended = () => {
          if (audioObjectUrlRef.current === url) revokeCurrentAudioUrl();
          reportStatus('语音已开启');
          setVoicePhase('idle');
          settle(null);
        };
        audio.onerror = () => {
          if (audioObjectUrlRef.current === url) revokeCurrentAudioUrl();
          const message = '语音播放失败';
          reportStatus(message);
          setVoicePhase('error');
          if (reportRuntimeStatus) notifyVoiceError('语音播报失败', message);
          settle(message);
        };
      });
      audio.onplay = () => {
        reportStatus('正在语音播报');
        setVoicePhase('playing');
      };
      try {
        await resumeAnalyserContext();
        await audio.play();
      } catch {
        const message = '语音播放被浏览器阻止';
        reportStatus(message);
        setVoicePhase('error');
        const pending = pendingPlaybackRef.current as PendingPlayback | null;
        if (pending?.requestId === requestId && !pending.settled) {
          pending.settled = true;
          pending.resolve(message);
          pendingPlaybackRef.current = null;
        }
        if (reportRuntimeStatus) notifyVoiceError('语音播报失败', message);
        return message;
      }
      if (waitUntilEnded) {
        return playbackFinished;
      }
      return null;
    },
    [
      notifyVoiceError,
      resumeAnalyserContext,
      revokeCurrentAudioUrl,
      setVoicePhase,
      voice.ttsAvailable,
      voice.voiceEnabled
    ]
  );

  const stopVoice = useCallback(() => {
    setVoicePhase('stopping');
    recordingRequestIdRef.current += 1;
    transcriptionRequestIdRef.current += 1;
    ttsRequestIdRef.current += 1;
    transcriptionAbortRef.current?.abort();
    transcriptionAbortRef.current = null;
    ttsAbortRef.current?.abort();
    ttsAbortRef.current = null;
    const recorder = detachPcmRecording();
    if (recorder) {
      void recorder.context.close().catch(() => undefined);
    }
    const audio = audioRef.current;
    if (audio) {
      audio.pause();
      audio.currentTime = 0;
      audio.onplay = null;
      audio.onended = null;
      audio.onerror = null;
      audio.removeAttribute('src');
      audio.load();
    }
    const pending = pendingPlaybackRef.current;
    if (pending && !pending.settled) {
      pending.settled = true;
      pending.resolve('语音播放已停止。');
    }
    pendingPlaybackRef.current = null;
    revokeCurrentAudioUrl();
    setVoicePhase('idle', { listening: false, status: '语音已停止' });
  }, [detachPcmRecording, revokeCurrentAudioUrl, setVoicePhase]);

  const toggleVoice = useCallback(
    (dialogue?: VoiceDialogueSnapshot) => {
      if (!voice.ttsAvailable) return;
      const nextEnabled = !voice.voiceEnabled;
      if (!nextEnabled) stopVoice();
      setVoice((value) => ({
        ...value,
        voiceEnabled: nextEnabled,
        status: nextEnabled ? '语音已开启' : '语音未开启'
      }));
      if (nextEnabled) {
        void resumeAnalyserContext().catch(() => {
          setVoice((value) => ({ ...value, status: '语音上下文启动失败' }));
        });
      }
      if (nextEnabled && dialogue?.role === 'assistant') {
        void speak(dialogue.text, { forceEnabled: true, voiceId: dialogue.voiceId });
      }
    },
    [resumeAnalyserContext, speak, stopVoice, voice.ttsAvailable, voice.voiceEnabled]
  );

  const stopPcmRecording = useCallback(async () => {
    const recordingRequestId = recordingRequestIdRef.current;
    const runtime = detachPcmRecording();
    if (!runtime) return;
    const inputSampleRate = runtime.context.sampleRate;
    await runtime.context.close();
    if (!mountedRef.current || recordingRequestIdRef.current !== recordingRequestId) return;
    setVoicePhase('transcribing', { listening: false, status: '正在识别语音' });

    const merged = mergePcmChunks(runtime.chunks);
    if (!merged.length) {
      setVoicePhase('idle');
      return;
    }
    const wav = encodePcm16Wav(resamplePcm(merged, inputSampleRate));
    const transcriptionId = transcriptionRequestIdRef.current + 1;
    transcriptionRequestIdRef.current = transcriptionId;
    transcriptionAbortRef.current?.abort();
    const controller = new AbortController();
    transcriptionAbortRef.current = controller;
    try {
      const transcript = await transcribeAudio(wav, 'audio/wav', controller.signal);
      if (
        !mountedRef.current ||
        controller.signal.aborted ||
        transcriptionRequestIdRef.current !== transcriptionId
      ) return;
      if (!transcript.trim()) {
        setVoicePhase('idle', { status: '没有识别到有效内容' });
        return;
      }
      const context = transcriptionContextRef.current;
      if (!context) return;
      transcriptRef.current(transcript, context);
      setVoicePhase('idle');
    } catch (err) {
      if (controller.signal.aborted || transcriptionRequestIdRef.current !== transcriptionId) return;
      const message = formatVoiceError(err, '语音识别失败');
      setVoicePhase('error', { status: '语音识别失败' });
      notifyVoiceError('语音识别失败', message);
    } finally {
      if (transcriptionAbortRef.current === controller) transcriptionAbortRef.current = null;
    }
  }, [detachPcmRecording, notifyVoiceError, setVoicePhase]);

  /// 开始语音输入：申请麦克风并直接采集 PCM。
  /// 再次点击（已在录音）时停止录音并上传识别。
  const startVoiceInput = useCallback(async () => {
    if (voicePhaseRef.current === 'recording') {
      await stopPcmRecording();
      return;
    }
    if (voicePhaseRef.current !== 'idle' && voicePhaseRef.current !== 'error') return;
    if (!voice.speechRecognitionAvailable) {
      setVoice((value) => ({ ...value, status: '语音识别服务未启用' }));
      return;
    }

    const requestId = recordingRequestIdRef.current + 1;
    recordingRequestIdRef.current = requestId;
    transcriptionContextRef.current = { ...getTranscriptContextRef.current() };
    setVoicePhase('requesting_permission', { status: '正在请求麦克风权限' });
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch (error) {
      if (recordingRequestIdRef.current !== requestId) return;
      const diagnostic = microphoneAccessError(error);
      setVoicePhase('error', { status: diagnostic.status });
      notifyVoiceError('语音输入失败', diagnostic.description);
      return;
    }
    if (!mountedRef.current || recordingRequestIdRef.current !== requestId) {
      stream.getTracks().forEach((track) => track.stop());
      return;
    }
    mediaStreamRef.current = stream;
    const context = new AudioContext();
    const source = context.createMediaStreamSource(stream);
    const processor = context.createScriptProcessor(4096, 1, 1);
    const silentOutput = context.createGain();
    silentOutput.gain.value = 0;
    const runtime: PcmRecorderRuntime = { context, source, processor, silentOutput, chunks: [] };
    processor.onaudioprocess = (event) => {
      runtime.chunks.push(new Float32Array(event.inputBuffer.getChannelData(0)));
    };
    source.connect(processor);
    processor.connect(silentOutput);
    silentOutput.connect(context.destination);
    pcmRecorderRef.current = runtime;
    setVoicePhase('recording', { listening: true, status: '正在听你说话' });
  }, [notifyVoiceError, setVoicePhase, stopPcmRecording, voice.speechRecognitionAvailable]);

  return {
    voice,
    audioRef,
    visualizerCanvasRef,
    speak,
    toggleVoice,
    stopVoice,
    startVoiceInput,
    refreshVoiceCapabilities
  };
}
