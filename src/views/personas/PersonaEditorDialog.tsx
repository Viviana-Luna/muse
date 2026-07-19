import { useState } from 'react';
import type { Dispatch, DragEvent, FormEvent, SetStateAction } from 'react';
import { createPortal } from 'react-dom';

import { uploadPersonaImage } from '@/api';
import { ApiError } from '@/api/client';
import type { AppToastInput } from '@/hooks/useAppToast';
import { useAuthenticatedAssetUrl } from '@/hooks/useAuthenticatedAsset';
import { useModalAccessibility } from '@/hooks/useModalAccessibility';
import { detectImageTheme } from '@/hooks/usePersonaTheme';
import type {
  ModelCatalog,
  ModelsConfig,
  Persona,
  PersonaModelReference,
  PersonaVisualPackPatch
} from '@/types';
import type {
  PersonaEditorMode,
  PersonaEditorState
} from '@/views/personas/hooks/usePersonaState';
import { PersonaImageCropDialog } from '@/views/personas/components/PersonaImageCropDialog';
import type { PersonaCropTargetKey } from '@/views/personas/utils/personaImageCrop';

const MAX_PERSONA_IMAGE_BYTES = 5 * 1024 * 1024;
const PERSONA_IMAGE_MIME_TYPES = ['image/png', 'image/jpeg', 'image/webp'];
const DEFAULT_PERSONA_THEME_COLOR = '#d8596f';
const DEFAULT_PORTRAIT_FRAME = 'portrait';
const DEFAULT_PORTRAIT_FIT = 'cover';
const DEFAULT_PORTRAIT_POSITION = 50;
const DEFAULT_PORTRAIT_SCALE = 100;

interface PersonaValidationIssue {
  field: keyof Persona;
  message: string;
}

interface PersonaEditorDialogProps {
  editor: PersonaEditorState;
  modelCatalog?: ModelCatalog | null;
  modelsConfig?: ModelsConfig | null;
  busy: boolean;
  notify: (input: AppToastInput) => void;
  onClose: () => void;
  onSave: (
    persona: Persona,
    mode: PersonaEditorMode,
    visualPack?: PersonaVisualPackPatch
  ) => Promise<boolean>;
  setEditor: Dispatch<SetStateAction<PersonaEditorState>>;
  updateEditor: <K extends keyof Persona>(key: K, value: Persona[K]) => void;
  updateEditorVisualDraft: <K extends keyof PersonaVisualPackPatch>(
    key: K,
    value: PersonaVisualPackPatch[K]
  ) => void;
  updateEditorToolPolicyMode: (mode: Persona['tool_policy']['mode']) => void;
  updateEditorAllowedTools: (value: string) => void;
  primaryActionLabel?: string;
}

function modelReferenceValue(reference: PersonaModelReference | null): string {
  return reference
    ? JSON.stringify([reference.provider_id, reference.model_id])
    : '';
}

function parseModelReference(value: string): PersonaModelReference | null {
  if (!value) return null;
  try {
    const parsed = JSON.parse(value) as unknown;
    if (
      Array.isArray(parsed) &&
      parsed.length === 2 &&
      parsed.every((item) => typeof item === 'string' && item.trim())
    ) {
      return { provider_id: parsed[0], model_id: parsed[1] };
    }
  } catch {
    // 选择值只由当前表单生成；解析失败时保持继承全局，避免写入残缺引用。
  }
  return null;
}

function trimDraftValue(value?: string): string | undefined {
  const trimmed = value?.trim() ?? '';
  return trimmed || undefined;
}

function buildVisualPackPatch(
  draft: PersonaVisualPackPatch
): PersonaVisualPackPatch {
  const portraitPath = trimDraftValue(draft.portrait_path);
  const avatarPath = trimDraftValue(draft.avatar_path);
  return {
    portrait_path: portraitPath ?? '',
    // 兼容字段继续存在，但新编辑流程显式清空旧角色背景。
    background_path: '',
    avatar_path: avatarPath,
    theme_color: trimDraftValue(draft.theme_color) ?? DEFAULT_PERSONA_THEME_COLOR,
    theme_mode:
      draft.theme_mode === 'dark' || draft.theme_mode === 'light' ? draft.theme_mode : 'auto',
    portrait_frame: DEFAULT_PORTRAIT_FRAME,
    portrait_fit: DEFAULT_PORTRAIT_FIT,
    portrait_position_x: DEFAULT_PORTRAIT_POSITION,
    portrait_position_y: DEFAULT_PORTRAIT_POSITION,
    portrait_scale: DEFAULT_PORTRAIT_SCALE
  };
}

function validatePersonaDraft(persona: Persona): PersonaValidationIssue | null {
  if (!persona.id.trim()) return { field: 'id', message: '请填写角色 ID。' };
  if (!/^[A-Za-z0-9_-]+$/u.test(persona.id.trim())) {
    return { field: 'id', message: '角色 ID 仅允许字母、数字、短横线和下划线。' };
  }
  if (!persona.name.trim()) return { field: 'name', message: '请填写角色名称。' };
  if (!persona.character_profile.trim()) {
    return { field: 'character_profile', message: '请填写角色特征。' };
  }
  if (!persona.system_prompt.trim()) {
    return { field: 'system_prompt', message: '请填写系统提示词。' };
  }
  if (!persona.default_visual_pack_id.trim()) {
    return { field: 'default_visual_pack_id', message: '请填写展示包 ID。' };
  }
  return null;
}

function normalizePersonaFieldErrors(fieldErrors: Record<string, string>) {
  return Object.fromEntries(
    Object.entries(fieldErrors).map(([field, message]) => [
      field.split('.').at(-1) ?? field,
      message
    ])
  );
}

export function PersonaEditorDialog({
  editor,
  modelCatalog = null,
  modelsConfig = null,
  busy,
  notify,
  onClose,
  onSave,
  setEditor,
  updateEditor,
  updateEditorVisualDraft,
  updateEditorToolPolicyMode,
  updateEditorAllowedTools,
  primaryActionLabel
}: PersonaEditorDialogProps) {
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [imageUploading, setImageUploading] = useState(false);
  const [cropSourceFile, setCropSourceFile] = useState<File | null>(null);
  const [themeDetecting, setThemeDetecting] = useState(false);
  const modalRef = useModalAccessibility<HTMLElement>(true, onClose);
  const portraitPath = editor.visualPackDraft.portrait_path.trim();
  const avatarPath = editor.visualPackDraft.avatar_path?.trim() ?? '';
  const portraitPreviewPath = useAuthenticatedAssetUrl(portraitPath);
  const avatarPreviewPath = useAuthenticatedAssetUrl(avatarPath);
  const preferredModelRef = editor.persona.preferred_model_ref;
  const preferredModelValue = modelReferenceValue(preferredModelRef);
  const enabledProviderIds = new Set(
    (modelCatalog?.providers ?? [])
      .filter((provider) => provider.enabled)
      .map((provider) => provider.id)
  );
  const chatModels = (modelCatalog?.models ?? []).filter(
    (model) =>
      model.enabled &&
      enabledProviderIds.has(model.provider_id) &&
      model.functions.includes('chat')
  );
  const preferredCatalogModel = preferredModelRef
    ? modelCatalog?.models.find(
        (model) =>
          model.provider_id === preferredModelRef.provider_id &&
          model.model === preferredModelRef.model_id
      )
    : undefined;
  const preferredChatModel = preferredModelRef
    ? chatModels.find(
        (model) =>
          model.provider_id === preferredModelRef.provider_id &&
          model.model === preferredModelRef.model_id
      )
    : undefined;
  const preferredProvider = preferredModelRef
    ? modelCatalog?.providers.find((provider) => provider.id === preferredModelRef.provider_id)
    : undefined;
  const preferredModelInvalid = Boolean(
    preferredModelRef &&
      modelCatalog &&
      (!preferredCatalogModel ||
        !preferredCatalogModel.enabled ||
        !preferredCatalogModel.functions.includes('chat') ||
        !preferredProvider?.enabled ||
        preferredProvider.status !== 'supported' ||
        !preferredProvider.api_key_configured)
  );
  const globalChatModel = modelsConfig?.chat;
  const globalModelLabel = globalChatModel?.provider && globalChatModel.model
    ? `${globalChatModel.provider} / ${globalChatModel.model}`
    : '尚未配置';
  const globalVoiceId = modelsConfig?.tts.voice_id.trim() || '尚未配置';
  const hasPortrait = portraitPath.length > 0;
  const visualPackIdPreview =
    editor.visualDirty
      ? `visual-${editor.persona.id || 'persona'}`
      : editor.persona.default_visual_pack_id;
  function focusField(field: string) {
    window.requestAnimationFrame(() => {
      modalRef.current?.querySelector<HTMLElement>(`[name="${field}"]`)?.focus();
    });
  }

  function updatePersona<K extends keyof Persona>(key: K, value: Persona[K]) {
    setFieldErrors((current) => {
      if (!current[key]) return current;
      const next = { ...current };
      delete next[key];
      return next;
    });
    updateEditor(key, value);
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const validationIssue = validatePersonaDraft(editor.persona);
    if (validationIssue) {
      setFieldErrors({ [validationIssue.field]: validationIssue.message });
      focusField(validationIssue.field);
      notify({
        title: '角色资料尚未完成',
        description: validationIssue.message,
        tone: 'warning'
      });
      return;
    }
    setFieldErrors({});
    try {
      const saved = await onSave(
        editor.persona,
        editor.mode,
        editor.visualDirty ? buildVisualPackPatch(editor.visualPackDraft) : undefined
      );
      if (!saved) return;
      onClose();
      notify({
        title: editor.mode === 'edit' ? '角色已更新' : '角色已创建',
        tone: 'success'
      });
    } catch (err) {
      if (err instanceof ApiError && Object.keys(err.fieldErrors).length > 0) {
        const nextErrors = normalizePersonaFieldErrors(err.fieldErrors);
        setFieldErrors(nextErrors);
        const firstField = Object.keys(nextErrors)[0];
        if (firstField) focusField(firstField);
      }
      notify({
        title: editor.mode === 'edit' ? '更新角色失败' : '创建角色失败',
        description: err instanceof Error ? err.message : '角色资料未能保存。',
        tone: 'error'
      });
    }
  }

  function selectCropSource(file?: File | null) {
    if (!file || busy) return;
    if (!PERSONA_IMAGE_MIME_TYPES.includes(file.type)) {
      notify({
        title: '图片格式不支持',
        description: '请选择 PNG、JPEG 或 WebP 图片。',
        tone: 'error'
      });
      return;
    }
    if (file.size > MAX_PERSONA_IMAGE_BYTES) {
      notify({ title: '图片过大', description: '角色图片不能超过 5MB。', tone: 'error' });
      return;
    }

    setCropSourceFile(file);
  }

  async function uploadCroppedImages(
    files: Record<PersonaCropTargetKey, File>
  ): Promise<boolean> {
    setImageUploading(true);
    try {
      const portrait = await uploadPersonaImage(files.portrait);
      const avatar = await uploadPersonaImage(files.avatar);
      const detectedTheme = await detectImageTheme(portrait.url).catch(() => undefined);
      setEditor((state) => ({
        ...state,
        visualPackDraft: {
          ...state.visualPackDraft,
          portrait_path: portrait.url,
          avatar_path: avatar.url,
          background_path: '',
          theme_color:
            detectedTheme?.themeColor ??
            state.visualPackDraft.theme_color ??
            DEFAULT_PERSONA_THEME_COLOR,
          theme_mode: detectedTheme?.themeMode ?? state.visualPackDraft.theme_mode ?? 'auto',
          portrait_frame: DEFAULT_PORTRAIT_FRAME,
          portrait_fit: DEFAULT_PORTRAIT_FIT,
          portrait_position_x: DEFAULT_PORTRAIT_POSITION,
          portrait_position_y: DEFAULT_PORTRAIT_POSITION,
          portrait_scale: DEFAULT_PORTRAIT_SCALE
        },
        visualDirty: true
      }));
      notify({
        title: '角色图片已更新',
        description: '已从同一原图生成 3:4 立绘和 1:1 头像。',
        tone: 'success'
      });
      return true;
    } catch (err) {
      notify({
        title: '上传图片失败',
        description: err instanceof Error ? err.message : '上传角色图片时遇到错误。',
        tone: 'error'
      });
      return false;
    } finally {
      setImageUploading(false);
    }
  }

  async function detectTheme() {
    if (!portraitPath) return;
    setThemeDetecting(true);
    try {
      const detectedTheme = await detectImageTheme(portraitPath);
      setEditor((state) => ({
        ...state,
        visualPackDraft: {
          ...state.visualPackDraft,
          theme_color: detectedTheme.themeColor,
          theme_mode: detectedTheme.themeMode
        },
        visualDirty: true
      }));
      notify({
        title: '主题色与明暗已识别',
        description: `${detectedTheme.themeColor} · ${detectedTheme.themeMode === 'light' ? '亮色界面' : '暗色界面'}`,
        tone: 'success'
      });
    } catch (err) {
      notify({
        title: '识别主题色失败',
        description: err instanceof Error ? err.message : '当前图片无法读取颜色。',
        tone: 'error'
      });
    } finally {
      setThemeDetecting(false);
    }
  }

  function handleImageDrop(event: DragEvent<HTMLLabelElement>) {
    event.preventDefault();
    selectCropSource(event.dataTransfer.files[0]);
  }

  const dialog = (
    <section
      className="modal-shell stacked role-editor-modal"
      ref={modalRef}
      tabIndex={-1}
      role="dialog"
      aria-modal="true"
      aria-label="角色编辑"
    >
      <form className="persona-form" noValidate onSubmit={(event) => void submit(event)}>
        <header>
          <div>
            <strong>
              {editor.mode === 'edit'
                ? '编辑角色'
                : editor.mode === 'copy'
                  ? '复制角色'
                  : '创建角色'}
            </strong>
            <span>保存后会同步本地角色库。</span>
          </div>
          <button type="button" onClick={onClose}>
            关闭
          </button>
        </header>
        <div className="persona-editor-layout">
          <section className="persona-image-panel" aria-label="角色形象">
            <div className="persona-image-source">
              <div>
                <strong>角色原图</strong>
                <span>选择一张图片，分别裁剪立绘和头像。</span>
              </div>
              <label
                className="persona-image-source-action"
                onDragOver={(event) => event.preventDefault()}
                onDrop={handleImageDrop}
              >
                <input
                  className="persona-file-input"
                  type="file"
                  aria-label="选择角色原图"
                  disabled={busy || imageUploading}
                  accept={PERSONA_IMAGE_MIME_TYPES.join(',')}
                  onChange={(event) => {
                    selectCropSource(event.target.files?.[0]);
                    event.target.value = '';
                  }}
                />
                {imageUploading
                  ? '正在上传'
                  : portraitPath || avatarPath
                    ? '重新选择并裁剪'
                    : '选择原图并裁剪'}
              </label>
            </div>
            <div className="persona-image-results" aria-label="角色图片裁剪结果">
              <figure className="persona-image-result is-portrait">
                {portraitPreviewPath ? (
                  <img src={portraitPreviewPath} alt="立绘预览" draggable={false} />
                ) : (
                  <span>暂无立绘</span>
                )}
                <figcaption>立绘 · 3:4</figcaption>
              </figure>
              <figure className="persona-image-result is-avatar">
                {avatarPreviewPath ? (
                  <img src={avatarPreviewPath} alt="头像预览" draggable={false} />
                ) : (
                  <span>暂无头像</span>
                )}
                <figcaption>头像 · 1:1</figcaption>
              </figure>
            </div>
            {!hasPortrait && !avatarPath && (
              <p className="persona-image-hint">
                支持 PNG、JPEG、WebP，原图不超过 5MB。裁剪结果固定为 900×1200 与 768×768。
              </p>
            )}
            <label>
              名称
              <input
                name="name"
                aria-invalid={Boolean(fieldErrors.name)}
                value={editor.persona.name}
                onChange={(event) => updatePersona('name', event.target.value)}
              />
              {fieldErrors.name && <span className="field-error" role="alert">{fieldErrors.name}</span>}
            </label>
            <label>
              说话风格
              <input
                value={editor.persona.style}
                onChange={(event) => updatePersona('style', event.target.value)}
              />
            </label>
            <label className="persona-color-field">
              主题色
              <span className="persona-color-inputs">
                <input
                  type="color"
                  value={editor.visualPackDraft.theme_color || DEFAULT_PERSONA_THEME_COLOR}
                  onChange={(event) => updateEditorVisualDraft('theme_color', event.target.value)}
                />
                <input
                  value={editor.visualPackDraft.theme_color || DEFAULT_PERSONA_THEME_COLOR}
                  onChange={(event) => updateEditorVisualDraft('theme_color', event.target.value)}
                />
                <button
                  type="button"
                  disabled={!hasPortrait || themeDetecting}
                  onClick={() => void detectTheme()}
                >
                  {themeDetecting ? '识别中' : '识别色彩与明暗'}
                </button>
              </span>
            </label>
            <label className="persona-theme-mode-field">
              界面明暗
              <span className="persona-theme-mode-options" role="group" aria-label="角色界面明暗">
                {[
                  ['auto', '自动', '根据角色图片亮度判断'],
                  ['dark', '暗色', '始终使用深色界面'],
                  ['light', '亮色', '始终使用浅色界面']
                ].map(([value, label, description]) => (
                  <button
                    type="button"
                    key={value}
                    className={(editor.visualPackDraft.theme_mode || 'auto') === value ? 'active' : ''}
                    onClick={() => updateEditorVisualDraft('theme_mode', value as 'auto' | 'dark' | 'light')}
                  >
                    <strong>{label}</strong>
                    <small>{description}</small>
                  </button>
                ))}
              </span>
            </label>
          </section>
          <section className="persona-editor-main" aria-label="角色资料">
            <label>
              摘要
              <textarea
                value={editor.persona.summary}
                onChange={(event) => updatePersona('summary', event.target.value)}
              />
            </label>
            <label>
              角色特征
              <textarea
                name="character_profile"
                aria-invalid={Boolean(fieldErrors.character_profile)}
                placeholder="例如：冷静、可靠，习惯用简洁但带有场景感的回应。"
                value={editor.persona.character_profile}
                onChange={(event) => updatePersona('character_profile', event.target.value)}
              />
              {fieldErrors.character_profile && (
                <span className="field-error" role="alert">{fieldErrors.character_profile}</span>
              )}
            </label>
            <label>
              世界观
              <textarea
                value={editor.persona.world_profile}
                onChange={(event) => updatePersona('world_profile', event.target.value)}
              />
            </label>
            <label>
              当前场景
              <textarea
                value={editor.persona.scenario}
                onChange={(event) => updatePersona('scenario', event.target.value)}
              />
            </label>
            <label>
              演绎风格
              <select
                value={editor.persona.roleplay_style}
                onChange={(event) => updatePersona('roleplay_style', event.target.value as Persona['roleplay_style'])}
              >
                <option value="dialogue">纯对白</option>
                <option value="light_narration">轻描写</option>
                <option value="immersive">沉浸描写</option>
                <option value="text_adventure">文字冒险</option>
              </select>
            </label>
            <label>
              演绎示例
              <textarea
                className="persona-large-textarea"
                value={editor.persona.dialogue_examples}
                onChange={(event) => updatePersona('dialogue_examples', event.target.value)}
              />
            </label>
            <label>
              演绎备注
              <textarea
                value={editor.persona.author_note}
                onChange={(event) => updatePersona('author_note', event.target.value)}
              />
            </label>
            <label>
              系统提示词
              <textarea
                className="persona-large-textarea"
                name="system_prompt"
                aria-invalid={Boolean(fieldErrors.system_prompt)}
                value={editor.persona.system_prompt}
                onChange={(event) => updatePersona('system_prompt', event.target.value)}
              />
              {fieldErrors.system_prompt && (
                <span className="field-error" role="alert">{fieldErrors.system_prompt}</span>
              )}
            </label>
            <label>
              开场白
              <input
                value={editor.persona.opening_message}
                onChange={(event) => updatePersona('opening_message', event.target.value)}
              />
            </label>
          </section>
        </div>
        <details className="persona-advanced">
          <summary>高级设置</summary>
          <div className="persona-advanced-grid">
            <label>
              角色 ID
              <input
                name="id"
                pattern={'[A-Za-z0-9_\\-]+'}
                aria-invalid={Boolean(fieldErrors.id)}
                value={editor.persona.id}
                disabled={editor.mode === 'edit'}
                onChange={(event) => updatePersona('id', event.target.value)}
              />
              {fieldErrors.id && <span className="field-error" role="alert">{fieldErrors.id}</span>}
            </label>
            <label>
              展示包 ID
              <input
                name="default_visual_pack_id"
                aria-invalid={Boolean(fieldErrors.default_visual_pack_id)}
                value={visualPackIdPreview}
                disabled={editor.visualDirty}
                onChange={(event) => updatePersona('default_visual_pack_id', event.target.value)}
              />
              {fieldErrors.default_visual_pack_id && (
                <span className="field-error" role="alert">{fieldErrors.default_visual_pack_id}</span>
              )}
            </label>
            <label>
              作者
              <input
                value={editor.persona.author}
                onChange={(event) => updatePersona('author', event.target.value)}
              />
            </label>
            <label>
              版本
              <input
                value={editor.persona.version}
                onChange={(event) => updatePersona('version', event.target.value)}
              />
            </label>
            <label>
              工具策略
              <select
                value={editor.persona.tool_policy.mode}
                onChange={(event) => updateEditorToolPolicyMode(event.target.value as Persona['tool_policy']['mode'])}
              >
                <option value="inherit">继承运行时</option>
                <option value="disabled">禁用工具</option>
                <option value="allow_list">白名单</option>
              </select>
            </label>
            <label>
              工具白名单
              <input
                value={editor.persona.tool_policy.allowed_tools.join(', ')}
                onChange={(event) => updateEditorAllowedTools(event.target.value)}
              />
            </label>
            <label>
              Skill 策略
              <select
                value={editor.persona.skill_policy.mode}
                onChange={(event) =>
                  updatePersona('skill_policy', {
                    ...editor.persona.skill_policy,
                    mode: event.target.value as Persona['skill_policy']['mode']
                  })
                }
              >
                <option value="inherit">继承运行时</option>
                <option value="disabled">禁用 Skill</option>
                <option value="allow_list">白名单</option>
              </select>
            </label>
            <label>
              Skill 白名单
              <input
                value={editor.persona.skill_policy.allowed_skills.join(', ')}
                onChange={(event) =>
                  updatePersona('skill_policy', {
                    ...editor.persona.skill_policy,
                    allowed_skills: event.target.value
                      .split(',')
                      .map((item) => item.trim())
                      .filter(Boolean)
                  })
                }
              />
            </label>
            <label>
              MCP 策略
              <select
                value={editor.persona.mcp_policy.mode}
                onChange={(event) =>
                  updatePersona('mcp_policy', {
                    ...editor.persona.mcp_policy,
                    mode: event.target.value as Persona['mcp_policy']['mode']
                  })
                }
              >
                <option value="inherit">继承运行时</option>
                <option value="disabled">禁用 MCP</option>
                <option value="allow_list">白名单</option>
              </select>
            </label>
            <label>
              MCP Server 白名单
              <input
                value={editor.persona.mcp_policy.allowed_servers.join(', ')}
                onChange={(event) =>
                  updatePersona('mcp_policy', {
                    ...editor.persona.mcp_policy,
                    allowed_servers: event.target.value
                      .split(',')
                      .map((item) => item.trim())
                      .filter(Boolean)
                  })
                }
              />
            </label>
            <label className="wide persona-runtime-reference-field">
              偏好聊天模型
              <select
                name="preferred_model_ref"
                aria-label="偏好聊天模型"
                value={preferredModelValue}
                onChange={(event) =>
                  updatePersona('preferred_model_ref', parseModelReference(event.target.value))
                }
              >
                <option value="">继承全局活动模型（{globalModelLabel}）</option>
                {preferredModelRef && !preferredChatModel && (
                  <option value={preferredModelValue} disabled>
                    当前引用已失效（{preferredModelRef.provider_id} / {preferredModelRef.model_id}）
                  </option>
                )}
                {chatModels.map((model) => {
                  const provider = modelCatalog?.providers.find(
                    (candidate) => candidate.id === model.provider_id
                  );
                  const available = Boolean(
                    provider?.enabled &&
                      provider.status === 'supported' &&
                      provider.api_key_configured &&
                      model.enabled
                  );
                  return (
                    <option
                      key={model.id}
                      value={modelReferenceValue({
                        provider_id: model.provider_id,
                        model_id: model.model
                      })}
                      disabled={!available}
                    >
                      {provider?.name ?? model.provider_id} / {model.name}
                      {available ? '' : '（不可用）'}
                    </option>
                  );
                })}
              </select>
              {preferredModelInvalid && (
                <span className="persona-reference-warning" role="status">
                  引用已失效或缺少 Provider 凭据。引用会继续保留；若全局活动模型可用，对话时将显式回退，否则会在请求前报错。请在此重新选择修复。
                </span>
              )}
              {!modelCatalog && (
                <small>模型目录暂不可用；已有引用不会被清空。</small>
              )}
            </label>
            <label className="wide persona-runtime-reference-field">
              偏好音色 ID
              <input
                name="preferred_voice_id"
                aria-label="偏好音色 ID"
                value={editor.persona.preferred_voice_id ?? ''}
                placeholder={`留空继承全局音色（${globalVoiceId}）`}
                onChange={(event) =>
                  updatePersona('preferred_voice_id', event.target.value || null)
                }
              />
              <small>
                音色 ID 由当前 TTS 供应商解释，本地不会伪造可用性；上游拒绝后不会静默切换音色。
              </small>
            </label>
            <label className="wide">
              备注
              <textarea
                value={editor.persona.notes}
                onChange={(event) => updatePersona('notes', event.target.value)}
              />
            </label>
          </div>
        </details>
        <footer>
          <button type="button" onClick={onClose}>取消</button>
          <button type="submit" disabled={busy || imageUploading || themeDetecting}>
            {primaryActionLabel || '保存'}
          </button>
        </footer>
      </form>
    </section>
  );

  const portalHost = document.querySelector<HTMLElement>('.runtime-shell') ?? document.body;
  return (
    <>
      {createPortal(dialog, portalHost)}
      {cropSourceFile && (
        <PersonaImageCropDialog
          file={cropSourceFile}
          busy={imageUploading}
          onCancel={() => setCropSourceFile(null)}
          onConfirm={uploadCroppedImages}
        />
      )}
    </>
  );
}
