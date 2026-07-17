import { createRef } from 'react';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Persona, PersonaLibraryItem } from '@/types';
import type { usePersonaTheme } from '@/hooks/usePersonaTheme';
import type { useChatRuntime } from '@/views/chat/hooks/useChatRuntime';
import { StoryWorkspace } from './StoryWorkspace';

const persona: Persona = {
  id: 'alice',
  name: '爱丽丝',
  summary: '一起探索故事',
  character_profile: '沉稳',
  world_profile: '雾都旧书店',
  scenario: '雨夜重逢',
  system_prompt: '保持角色。',
  style: '',
  roleplay_style: 'light_narration',
  dialogue_examples: '',
  author_note: '',
  opening_message: '你终于来了。',
  tool_policy: { mode: 'inherit', allowed_tools: [] },
  skill_policy: { mode: 'inherit', allowed_skills: [] },
  mcp_policy: { mode: 'inherit', allowed_servers: [] },
  preferred_model_ref: null,
  preferred_voice_id: null,
  default_visual_pack_id: 'alice-visual',
  author: 'Muse',
  version: '1.0.0',
  notes: ''
};

const summary: PersonaLibraryItem = {
  id: persona.id,
  name: persona.name,
  summary: persona.summary,
  default_visual_pack_id: persona.default_visual_pack_id,
  author: persona.author,
  version: persona.version,
  visual_preview: {
    avatar_path: null,
    portrait_path: null
  }
};

function createRuntime(overrides: Record<string, unknown> = {}) {
  return {
    modelLabel: 'mock / model',
    dialogue: { speaker: persona.name, text: persona.opening_message, role: 'assistant' },
    inputValue: '',
    setInputValue: vi.fn(),
    skillPicker: {
      open: false,
      skills: [],
      selectedSkill: null,
      loading: false,
      error: null,
      omittedSkillCount: 0,
      openPicker: vi.fn(),
      closePicker: vi.fn(),
      selectSkill: vi.fn(),
      clearSelection: vi.fn(),
      refresh: vi.fn()
    },
    sessionPanelOpen: false,
    setSessionPanelOpen: vi.fn(),
    activeConversationId: 'default',
    selectedConversationId: 'default',
    runtimeSessions: [],
    selectedRuntimeSession: undefined,
    selectedConversationReadOnly: false,
    runtimeTokenUsage: null,
    runtimeContextSnapshot: null,
    messages: [],
    busy: false,
    canceling: false,
    canCancel: false,
    resolveApproval: vi.fn(),
    resolveUserQuestion: vi.fn(),
    cancelCurrentTurn: vi.fn(),
    voice: {
      ttsAvailable: false,
      speechRecognitionAvailable: false,
      voiceEnabled: false,
      listening: false,
      phase: 'idle',
      status: '语音未开启'
    },
    audioRef: createRef<HTMLAudioElement>(),
    visualizerCanvasRef: createRef<HTMLCanvasElement>(),
    toggleVoice: vi.fn(),
    stopVoice: vi.fn(),
    startVoiceInput: vi.fn(),
    chatScrollRef: createRef<HTMLDivElement>(),
    hasUnreadUpdate: false,
    handleChatScroll: vi.fn(),
    scrollToLatest: vi.fn(),
    handleSend: vi.fn(),
    handleResumeSession: vi.fn(),
    handleForkSession: vi.fn(),
    handleDeleteRuntimeSession: vi.fn(),
    handleSelectRuntimeSession: vi.fn(),
    selectSessionFromHistory: vi.fn(),
    openSessionHistory: vi.fn(),
    startNewConversationFromHistory: vi.fn(),
    retryBootstrap: vi.fn(),
    ...overrides
  } as unknown as ReturnType<typeof useChatRuntime>;
}

const theme = {
  backgroundPath: '/assets/alice-background.png',
  portraitPath: '/assets/alice-portrait.png',
  avatarPath: '/assets/alice-portrait.png',
  stageStateClass: 'is-idle',
  themeColor: '#887766',
  themeMode: 'dark',
  rootStyle: {}
} as unknown as ReturnType<typeof usePersonaTheme>;

const themeWithoutImage = {
  ...theme,
  backgroundPath: '',
  portraitPath: '',
  avatarPath: ''
} as ReturnType<typeof usePersonaTheme>;

function renderWorkspace(
  resourceState: Parameters<typeof StoryWorkspace>[0]['resourceState'],
  options: {
    runtime?: ReturnType<typeof useChatRuntime>;
    theme?: ReturnType<typeof usePersonaTheme>;
    transitionState?: Parameters<typeof StoryWorkspace>[0]['transitionState'];
  } = {}
) {
  const onCreatePersona = vi.fn();
  const onImportPersona = vi.fn();
  const onOpenPersonaLibrary = vi.fn();
  const onOpenDiagnostics = vi.fn();
  const onOpenSessions = vi.fn();
  const runtime = options.runtime ?? createRuntime();
  const view = render(
    <StoryWorkspace
      resourceState={resourceState}
      transitionState={options.transitionState ?? { status: 'idle' }}
      runtime={runtime}
      theme={options.theme ?? theme}
      onCreatePersona={onCreatePersona}
      onImportPersona={onImportPersona}
      onOpenPersonaLibrary={onOpenPersonaLibrary}
      onOpenDiagnostics={onOpenDiagnostics}
      onOpenSessions={onOpenSessions}
    />
  );
  return {
    ...view,
    runtime,
    onCreatePersona,
    onImportPersona,
    onOpenPersonaLibrary,
    onOpenDiagnostics,
    onOpenSessions
  };
}

describe('StoryWorkspace', () => {
  afterEach(() => {
    cleanup();
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 });
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: 768 });
  });

  it('640×400 视口仍保留可聚焦的创建与导入主操作', () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 640 });
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: 400 });
    renderWorkspace({
      status: 'ready',
      data: {
        stateRevision: 1,
        personas: { personas: [], active_persona_id: null },
        activePersonaId: null,
        active: null
      }
    });

    const statusCard = document.querySelector('.story-status-card');
    expect(statusCard).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '创建角色' })).toBeEnabled();
    expect(screen.getByRole('button', { name: '导入角色卡' })).toBeEnabled();
  });

  it('空角色库只展示真实空状态和两个明确入口', () => {
    const view = renderWorkspace({
      status: 'ready',
      data: {
        stateRevision: 1,
        personas: { personas: [], active_persona_id: null },
        activePersonaId: null,
        active: null
      }
    });

    expect(screen.getByRole('heading', { name: '暂无角色' })).toBeInTheDocument();
    expect(screen.getByText('创建或导入一个角色后，即可开始对话。')).toBeInTheDocument();
    expect(screen.queryByText('现实日常')).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(document.querySelector('.story-neutral-stage img')).toBeNull();
    expect(document.querySelectorAll('.story-status-actions button')).toHaveLength(2);

    fireEvent.click(screen.getByRole('button', { name: '创建角色' }));
    fireEvent.click(screen.getByRole('button', { name: '导入角色卡' }));
    expect(view.onCreatePersona).toHaveBeenCalledOnce();
    expect(view.onImportPersona).toHaveBeenCalledOnce();
  });

  it('有角色但未激活时引导前往角色库，不伪造当前角色', () => {
    const view = renderWorkspace({
      status: 'ready',
      data: {
        stateRevision: 1,
        personas: { personas: [summary], active_persona_id: null },
        activePersonaId: null,
        active: null
      }
    });

    expect(screen.getByRole('heading', { name: '尚未选择角色' })).toBeInTheDocument();
    expect(screen.getByText('角色库中已有 1 个角色，请先选择并启用一个角色。')).toBeInTheDocument();
    expect(screen.queryByText(persona.name)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '前往角色库' }));
    expect(view.onOpenPersonaLibrary).toHaveBeenCalledOnce();
  });

  it('无活动角色时把历史入口导航到独立会话页', () => {
    const session = {
      conversation_id: 'history-a',
      exists: true,
      can_resume: true,
      records: 2,
      summary: '此前的故事'
    };
    const runtime = createRuntime({ runtimeSessions: [session] });
    const view = renderWorkspace(
      {
        status: 'ready',
        data: {
          stateRevision: 1,
          personas: { personas: [summary], active_persona_id: null },
          activePersonaId: null,
          active: null
        }
      },
      { runtime }
    );

    fireEvent.click(screen.getByRole('button', { name: '查看历史会话' }));
    expect(view.onOpenSessions).toHaveBeenCalledOnce();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('历史会话列表')).not.toBeInTheDocument();
  });

  it('加载与失败均不会泄漏默认角色内容，并提供可恢复操作', () => {
    const loading = renderWorkspace({ status: 'loading' });
    expect(screen.getByLabelText('正在读取角色状态')).toHaveAttribute('aria-busy', 'true');
    expect(screen.queryByText('暂无角色')).not.toBeInTheDocument();
    loading.unmount();

    const runtime = createRuntime();
    const failed = renderWorkspace(
      { status: 'failed', error: '本地服务暂不可用' },
      { runtime }
    );
    expect(screen.getByRole('heading', { name: '无法读取角色状态' })).toBeInTheDocument();
    expect(screen.getByText('本地服务暂不可用')).toBeInTheDocument();
    expect(screen.queryByText('暂无角色')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    fireEvent.click(screen.getByRole('button', { name: '打开系统诊断' }));
    expect(runtime.retryBootstrap).toHaveBeenCalledOnce();
    expect(failed.onOpenDiagnostics).toHaveBeenCalledOnce();
  });

  it('活动角色 ready 状态保持剧情、舞台与输入能力回归', () => {
    renderWorkspace({
      status: 'ready',
      data: {
        stateRevision: 1,
        personas: { personas: [summary], active_persona_id: persona.id },
        activePersonaId: persona.id,
        active: { persona, visual_pack: null }
      }
    });

    expect(screen.getByRole('button', { name: `从“${persona.scenario}”开始` })).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveAttribute(
      'placeholder',
      `对${persona.name}说点什么…`
    );
    expect(screen.getByAltText(`${persona.name}的立绘`)).toHaveAttribute(
      'src',
      '/assets/alice-portrait.png'
    );
    expect(screen.queryByText('暂无角色')).not.toBeInTheDocument();
    expect(document.querySelector('.story-scene-head')).not.toBeInTheDocument();
  });

  it('聊天记录区域不再渲染会话仪表头，全部高度留给消息', () => {
    const runtime = createRuntime({
      activeConversationId: 'conversation-a',
      selectedConversationId: 'conversation-a',
      runtimeSessions: [
        {
          conversation_id: 'conversation-a',
          exists: true,
          can_resume: true,
          records: 2,
          summary: '雨夜重逢'
        }
      ],
      messages: [
        {
          id: 'user-message',
          role: 'user',
          content: '你好',
          status: 'completed',
          process: []
        }
      ]
    });
    renderWorkspace(
      {
        status: 'ready',
        data: {
          stateRevision: 1,
          personas: { personas: [summary], active_persona_id: persona.id },
          activePersonaId: persona.id,
          active: { persona, visual_pack: null }
        }
      },
      { runtime }
    );

    expect(screen.getByText('你好')).toBeInTheDocument();
    expect(screen.queryByText('当前对话')).not.toBeInTheDocument();
    expect(screen.queryByText('雨夜重逢')).not.toBeInTheDocument();
    expect(document.querySelector('.story-scene-head')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /切换当前模型/ })).not.toBeInTheDocument();
  });

  it('首次发送前直接展示角色开场，不占用额外仪表头', () => {
    const runtime = createRuntime({ messages: [] });
    renderWorkspace(
      {
        status: 'ready',
        data: {
          stateRevision: 1,
          personas: { personas: [summary], active_persona_id: persona.id },
          activePersonaId: persona.id,
          active: { persona, visual_pack: null }
        }
      },
      { runtime }
    );

    expect(screen.getByText(`${persona.name}正在这里`)).toBeInTheDocument();
    expect(screen.queryByText('等待首条消息')).not.toBeInTheDocument();
    expect(document.querySelector('.story-scene-head')).not.toBeInTheDocument();
  });

  it('活动角色没有立绘时展示无图状态，不注入默认人物图片', () => {
    renderWorkspace(
      {
        status: 'ready',
        data: {
          stateRevision: 1,
          personas: { personas: [summary], active_persona_id: persona.id },
          activePersonaId: persona.id,
          active: { persona, visual_pack: null }
        }
      },
      { theme: themeWithoutImage }
    );

    expect(screen.getByRole('img', { name: `${persona.name}暂无角色图片` })).toBeVisible();
    expect(screen.getByText('暂无角色图片')).toBeVisible();
    expect(document.querySelector('.story-stage-panel img')).toBeNull();
  });

  it('活动角色查看只读历史时提供恢复与分叉继续入口', () => {
    const session = {
      conversation_id: 'history-a',
      exists: true,
      can_resume: true,
      records: 2
    };
    const runtime = createRuntime({
      activeConversationId: 'default',
      selectedConversationId: 'history-a',
      selectedRuntimeSession: session,
      selectedConversationReadOnly: true,
      runtimeSessions: [session]
    });
    renderWorkspace(
      {
        status: 'ready',
        data: {
          stateRevision: 1,
          personas: { personas: [summary], active_persona_id: persona.id },
          activePersonaId: persona.id,
          active: { persona, visual_pack: null }
        }
      },
      { runtime }
    );

    expect(screen.getByRole('textbox', { name: '历史会话只读输入' })).toHaveAttribute('readonly');
    fireEvent.click(screen.getByRole('button', { name: '恢复并继续' }));
    fireEvent.click(screen.getByRole('button', { name: '分叉后继续' }));
    expect(runtime.handleResumeSession).toHaveBeenCalledOnce();
    expect(runtime.handleForkSession).toHaveBeenCalledOnce();
  });

  it('角色切换期间用明确遮罩保留旧可信画面，禁止输入', () => {
    const runtime = createRuntime({
      messages: [
        {
          id: 'old-message',
          role: 'assistant',
          content: '旧角色的可信记录',
          status: 'completed',
          process: []
        }
      ]
    });
    renderWorkspace(
      {
        status: 'refreshing',
        data: {
          stateRevision: 1,
          personas: { personas: [summary], active_persona_id: persona.id },
          activePersonaId: persona.id,
          active: { persona, visual_pack: null }
        }
      },
      { runtime, transitionState: { status: 'pending' } }
    );

    expect(screen.getByText('旧角色的可信记录')).toBeInTheDocument();
    expect(screen.getByText('正在同步角色与对话')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveAttribute('readonly');
    expect(screen.getByRole('textbox')).toHaveAttribute(
      'placeholder',
      '正在同步角色与对话，请稍候。'
    );
  });

  it.each([
    ['refreshing', '角色状态正在同步，请等待完成后再操作。'],
    ['stale', '角色状态已过期，请先重试同步。']
  ] as const)('%s 状态禁用发送并保持聊天记录可读', (status, reason) => {
    const activeData = {
      stateRevision: 1,
      personas: { personas: [summary], active_persona_id: persona.id },
      activePersonaId: persona.id,
      active: { persona, visual_pack: null }
    };
    const resourceState =
      status === 'stale'
        ? ({ status, data: activeData, error: '同步失败' } as const)
        : ({ status, data: activeData } as const);
    const composer = renderWorkspace(resourceState);
    expect(screen.getByRole('textbox')).toHaveAttribute('readonly');
    expect(screen.getByRole('textbox')).toHaveAttribute('placeholder', reason);
    expect(document.querySelector('.story-scene-head')).not.toBeInTheDocument();
    expect(composer.onOpenSessions).not.toHaveBeenCalled();
  });
});
