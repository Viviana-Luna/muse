import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import {
  ArrowDown,
  BookOpenText,
  BookUser,
  CircleAlert,
  ImageOff,
  LibraryBig,
  RefreshCw,
  SendHorizontal,
  Upload,
  UserRoundPlus
} from 'lucide-react';

import { ComposerBar } from '@/views/chat/components/ComposerBar';
import { GalgameDialogueBox } from '@/views/chat/components/GalgameDialogueBox';
import { ClassicInteractionPanel } from '@/views/chat/components/PendingInteractionPanel';
import { StageView } from '@/views/chat/components/StageView';
import type { useChatRuntime } from '@/views/chat/hooks/useChatRuntime';
import type { usePersonaTheme } from '@/hooks/usePersonaTheme';
import {
  derivePersonaPresence,
  type OperationState,
  type ResourceState,
  type StoryPersonaSnapshot
} from '@/views/story/types';

type ChatRuntime = ReturnType<typeof useChatRuntime>;
type PersonaTheme = ReturnType<typeof usePersonaTheme>;

interface StoryWorkspaceProps {
  resourceState: ResourceState<StoryPersonaSnapshot>;
  transitionState: OperationState;
  runtime: ChatRuntime;
  theme: PersonaTheme;
  onCreatePersona: () => void;
  onImportPersona: () => void;
  onOpenPersonaLibrary: () => void;
  onOpenDiagnostics: () => void;
  onOpenSessions: () => void;
}

function useDelayedSkeleton(enabled: boolean) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (!enabled) {
      setVisible(false);
      return;
    }
    const timer = window.setTimeout(() => setVisible(true), 250);
    return () => window.clearTimeout(timer);
  }, [enabled]);

  return visible;
}

function LoadingWorkspace() {
  const skeletonVisible = useDelayedSkeleton(true);

  return (
    <section
      className="story-workspace story-system-workspace is-loading"
      aria-label="正在读取角色状态"
      aria-busy="true"
    >
      <div className="story-reading-column story-loading-card">
        {skeletonVisible && (
          <div className="story-loading-content" aria-hidden="true">
            <span className="story-skeleton short" />
            <span className="story-skeleton title" />
            <span className="story-skeleton body" />
            <span className="story-skeleton action" />
            <span className="story-skeleton action secondary" />
          </div>
        )}
      </div>
      <aside className="story-stage-panel story-neutral-stage story-loading-stage" aria-hidden="true">
        {skeletonVisible && <span className="story-skeleton stage-mark" />}
      </aside>
    </section>
  );
}

interface NeutralStageProps {
  icon: 'empty' | 'error';
  label: string;
}

function NeutralStage({ icon, label }: NeutralStageProps) {
  const Icon = icon === 'error' ? CircleAlert : BookUser;
  return (
    <aside className="story-stage-panel story-neutral-stage" aria-label={label}>
      <Icon aria-hidden="true" />
    </aside>
  );
}

interface StatusCardProps {
  eyebrow: string;
  title: string;
  description: string;
  children: ReactNode;
  tone?: 'neutral' | 'error';
}

function StatusCard({ eyebrow, title, description, children, tone = 'neutral' }: StatusCardProps) {
  return (
    <div className={`story-reading-column story-status-card tone-${tone}`}>
      <div className="story-status-card-content">
        <span className="story-status-eyebrow">{eyebrow}</span>
        <h1>{title}</h1>
        <p>{description}</p>
        <div className="story-status-actions">{children}</div>
      </div>
    </div>
  );
}

interface ActiveWorkspaceProps
  extends Pick<
    StoryWorkspaceProps,
    'runtime' | 'theme'
  > {
  active: NonNullable<StoryPersonaSnapshot['active']>;
  syncing: boolean;
  transitioning: boolean;
  mutationLockReason: string | null;
  staleError?: string;
}

function ActiveWorkspace({
  active,
  runtime,
  theme,
  syncing,
  transitioning,
  mutationLockReason,
  staleError
}: ActiveWorkspaceProps) {
  const {
    dialogue,
    inputValue,
    setInputValue,
    skillPicker,
    selectedRuntimeSession,
    selectedConversationReadOnly,
    messages,
    busy,
    canceling,
    canCancel,
    resolveApproval,
    resolveUserQuestion,
    cancelCurrentTurn,
    voice,
    audioRef,
    visualizerCanvasRef,
    toggleVoice,
    stopVoice,
    startVoiceInput,
    chatScrollRef,
    hasUnreadUpdate,
    handleChatScroll,
    scrollToLatest,
    handleSend,
    handleResumeSession,
    handleForkSession,
    retryBootstrap
  } = runtime;
  const {
    backgroundPath,
    portraitPath,
    stageStateClass
  } = theme;
  const [portraitFailed, setPortraitFailed] = useState(false);

  useEffect(() => setPortraitFailed(false), [portraitPath]);

  const hasPortrait = Boolean(portraitPath) && !portraitFailed;
  const persona = active.persona;

  return (
    <>
      <audio ref={audioRef} className="voice-player" preload="auto" />
      <StageView
        stageStateClass={stageStateClass}
        backgroundPath={backgroundPath}
        portraitPath={portraitPath}
        personaName={persona.name}
        showPortrait={false}
      />
      <section
        className="story-workspace"
        aria-label="角色剧情"
        aria-busy={syncing || undefined}
      >
        {!transitioning && (syncing || staleError) && (
          <div className={`story-sync-notice ${staleError ? 'is-error' : ''}`} role="status">
            {staleError ? (
              <>
                <span>角色状态同步失败，当前仍显示上一次可信内容。</span>
                <button type="button" onClick={retryBootstrap}>重试</button>
              </>
            ) : (
              <span>正在同步角色状态…</span>
            )}
          </div>
        )}
        <div className={`story-reading-column ${selectedConversationReadOnly ? 'read-only' : ''}`}>
          <div className="story-scroll-region" ref={chatScrollRef} onScroll={handleChatScroll}>
            <GalgameDialogueBox
              messages={messages}
              scrollContainerRef={chatScrollRef}
              activePersonaName={persona.name}
              activePersonaScenario={persona.scenario}
              emptyText={persona.opening_message || '从一句问候开始，让故事自然发生。'}
              onUsePrompt={setInputValue}
            />
            {!mutationLockReason && (
              <ClassicInteractionPanel
                messages={messages}
                onResolveApproval={resolveApproval}
                onResolveUserQuestion={resolveUserQuestion}
              />
            )}
          </div>
          {!selectedConversationReadOnly && (
            <ComposerBar
              busy={busy}
              canceling={canceling}
              canCancel={canCancel}
              inputValue={inputValue}
              skillPicker={skillPicker}
              voice={voice}
              visualizerCanvasRef={visualizerCanvasRef}
              sendIcon={<SendHorizontal aria-hidden="true" />}
              sendDisabledReason={mutationLockReason || undefined}
              placeholder={`对${persona.name}说点什么…`}
              onInputValueChange={setInputValue}
              onSend={handleSend}
              onStartVoiceInput={startVoiceInput}
              onToggleVoice={() => toggleVoice(dialogue)}
              onStopVoice={stopVoice}
              onCancel={cancelCurrentTurn}
            />
          )}
          {selectedConversationReadOnly && (
            <section className="dialogue-panel story-history-continuation" aria-label="继续历史会话">
              <textarea
                readOnly
                aria-label="历史会话只读输入"
                placeholder="这是只读历史记录，恢复或分叉后即可继续输入。"
              />
              <div>
                <span>{selectedRuntimeSession?.can_resume ? '选择继续方式' : '该历史会话无法继续'}</span>
                <button
                  type="button"
                  className="primary"
                  disabled={busy || Boolean(mutationLockReason) || !selectedRuntimeSession?.can_resume}
                  onClick={() => void handleResumeSession()}
                >
                  恢复并继续
                </button>
                <button
                  type="button"
                  disabled={busy || Boolean(mutationLockReason) || !selectedRuntimeSession?.can_resume}
                  onClick={() => void handleForkSession()}
                >
                  分叉后继续
                </button>
              </div>
            </section>
          )}
          {!selectedConversationReadOnly && hasUnreadUpdate && (
            <button
              type="button"
              className="story-latest-action"
              onClick={() => scrollToLatest('smooth')}
              aria-label="有新回复，点击回到最新消息"
            >
              <ArrowDown aria-hidden="true" />
              <span>有新回复</span>
              <small>回到最新</small>
            </button>
          )}
        </div>
        <aside
          className={`story-stage-panel${hasPortrait ? '' : ' is-empty'}`}
          aria-label={`${persona.name}的舞台`}
        >
          {hasPortrait ? (
            <img
              className="story-stage-portrait"
              src={portraitPath}
              alt={`${persona.name}的立绘`}
              draggable={false}
              onError={() => setPortraitFailed(true)}
            />
          ) : (
            <div className="story-stage-empty" role="img" aria-label={`${persona.name}暂无角色图片`}>
              <ImageOff aria-hidden="true" />
              <span>暂无角色图片</span>
            </div>
          )}
        </aside>
        {transitioning && (
          <div className="story-transition-mask" role="status" aria-live="polite">
            <RefreshCw aria-hidden="true" />
            <strong>正在同步角色与对话</strong>
            <span>完成前会保留上一次可信画面，避免角色与聊天记录混合。</span>
          </div>
        )}
      </section>
    </>
  );
}

export function StoryWorkspace({
  resourceState,
  transitionState,
  runtime,
  theme,
  onCreatePersona,
  onImportPersona,
  onOpenPersonaLibrary,
  onOpenDiagnostics,
  onOpenSessions
}: StoryWorkspaceProps) {
  if (resourceState.status === 'initial' || resourceState.status === 'loading') {
    return <LoadingWorkspace />;
  }

  if (resourceState.status === 'failed') {
    return (
      <section className="story-workspace story-system-workspace" aria-label="角色状态读取失败">
        <StatusCard
          eyebrow="角色空间"
          title="无法读取角色状态"
          description={resourceState.error}
          tone="error"
        >
          <button type="button" className="primary" onClick={runtime.retryBootstrap}>
            <RefreshCw aria-hidden="true" />
            重试
          </button>
          <button type="button" onClick={onOpenDiagnostics}>
            <CircleAlert aria-hidden="true" />
            打开系统诊断
          </button>
        </StatusCard>
        <NeutralStage icon="error" label="角色状态读取失败" />
      </section>
    );
  }

  const presence = derivePersonaPresence(resourceState);
  const transitioning = transitionState.status === 'pending';
  const syncing = resourceState.status === 'refreshing' || transitioning;
  const staleError = resourceState.status === 'stale' ? resourceState.error : undefined;
  const mutationLockReason = transitioning
    ? '正在同步角色与对话，请稍候。'
    : resourceState.status === 'refreshing'
      ? '角色状态正在同步，请等待完成后再操作。'
      : resourceState.status === 'stale'
        ? '角色状态已过期，请先重试同步。'
        : null;

  if (presence?.kind === 'active_persona') {
    return (
      <ActiveWorkspace
        active={presence.active}
        runtime={runtime}
        theme={theme}
        syncing={syncing}
        transitioning={transitioning}
        mutationLockReason={mutationLockReason}
        staleError={staleError}
      />
    );
  }

  const isEmptyLibrary = presence?.kind === 'empty_library';
  const hasReadableHistory = runtime.runtimeSessions.some(
    (session) => session.can_resume && session.records > 0
  );
  return (
    <section
      className="story-workspace story-system-workspace"
      aria-label={isEmptyLibrary ? '角色库为空' : '尚未选择角色'}
      aria-busy={syncing || undefined}
    >
      {staleError && (
        <div className="story-sync-notice is-error" role="status">
          <span>角色状态同步失败，当前仍显示上一次可信内容。</span>
          <button type="button" onClick={runtime.retryBootstrap}>重试</button>
        </div>
      )}
      {isEmptyLibrary ? (
        <StatusCard
          eyebrow="角色空间"
          title="暂无角色"
          description="创建或导入一个角色后，即可开始对话。"
        >
          <button
            type="button"
            className="primary"
            disabled={Boolean(mutationLockReason)}
            onClick={onCreatePersona}
          >
            <UserRoundPlus aria-hidden="true" />
            创建角色
          </button>
          <button type="button" disabled={Boolean(mutationLockReason)} onClick={onImportPersona}>
            <Upload aria-hidden="true" />
            导入角色卡
          </button>
          {hasReadableHistory && (
            <button type="button" onClick={onOpenSessions}>
              <BookOpenText aria-hidden="true" />
              查看历史会话
            </button>
          )}
        </StatusCard>
      ) : (
        <StatusCard
          eyebrow="角色空间"
          title="尚未选择角色"
          description={`角色库中已有 ${presence?.kind === 'no_active_persona' ? presence.personaCount : 0} 个角色，请先选择并启用一个角色。`}
        >
          <button type="button" className="primary" onClick={onOpenPersonaLibrary}>
            <LibraryBig aria-hidden="true" />
            前往角色库
          </button>
          <button type="button" disabled={Boolean(mutationLockReason)} onClick={onCreatePersona}>
            <UserRoundPlus aria-hidden="true" />
            创建新角色
          </button>
          {hasReadableHistory && (
            <button type="button" onClick={onOpenSessions}>
              <BookOpenText aria-hidden="true" />
              查看历史会话
            </button>
          )}
        </StatusCard>
      )}
      <NeutralStage icon="empty" label="等待选择角色的空舞台" />
      {transitioning && (
        <div className="story-transition-mask" role="status" aria-live="polite">
          <RefreshCw aria-hidden="true" />
          <strong>正在同步角色与对话</strong>
          <span>完成前会保留上一次可信画面。</span>
        </div>
      )}
    </section>
  );
}
