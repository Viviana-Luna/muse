/**
 * 设置中心主对话框（SettingsDialog）。
 *
 * 这是 Muse 应用「设置中心」的壳层组件，负责渲染一个包含左侧分类导航和
 * 右侧面板内容的模态对话框。它聚合了 7 个设置面板：
 * 对话模型、语音合成、语音识别、工具工作区、联网搜索、外观配置、系统诊断。
 *
 * 本组件只负责：面板切换、脏数据（未保存更改）提示、
 * 关闭确认、统一保存提交。各面板的实际表单内容由 ./panels/* 下的子组件渲染，
 * 表单状态由父组件通过 props 注入。
 *
 * 通过 embedded prop 切换两种形态：
 * - embedded=true：作为内嵌区域渲染（region 角色），无头部、不拦截背景点击；
 * - embedded=false（默认）：作为模态对话框渲染，带遮罩点击关闭、Esc 关闭等行为。
 */
import { useCallback, useState } from 'react';
import * as AlertDialog from '@radix-ui/react-alert-dialog';
import {
  Activity,
  AudioLines,
  Bot,
  ChevronLeft,
  Globe2,
  Mic2,
  Palette,
  ShieldCheck
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useModalAccessibility } from '@/hooks/useModalAccessibility';

import { AppearancePanel } from './panels/AppearancePanel';
import { ChatPanel } from './panels/ChatPanel';
import { DiagnosticsPanel } from './panels/DiagnosticsPanel';
import { SpeechRecognitionPanel } from './panels/SpeechRecognitionPanel';
import { TtsPanel } from './panels/TtsPanel';
import { WorkspacePanel } from './panels/WorkspacePanel';
import { WebSearchPanel } from './panels/WebSearchPanel';
import type { SettingsDialogProps, SettingsPanel, SettingsPanelDefinition } from './types';
export type { AppearanceSettings, SettingsPanel } from './types';

const SETTINGS_PANELS: SettingsPanelDefinition[] = [
  {
    id: 'chat',
    label: '对话模型',
    description: '管理供应商凭据、模型目录与活动聊天模型。',
    marker: '对'
  },
  {
    id: 'tts',
    label: '语音合成',
    description: '配置文本转语音服务、声音与试听参数。',
    marker: '音'
  },
  {
    id: 'speech_recognition',
    label: '语音识别',
    description: '配置远程语音转写服务与识别模型。',
    marker: '识'
  },
  {
    id: 'workspace',
    label: '工具工作区',
    description: '设置工具执行、审批与文件访问边界。',
    marker: '权'
  },
  {
    id: 'web_search',
    label: '联网搜索',
    description: '配置联网搜索凭据与可用状态。',
    marker: '网'
  },
  {
    id: 'appearance',
    label: '外观配置',
    description: '调整界面动效强度与可访问性偏好。',
    marker: '效'
  },
  {
    id: 'diagnostics',
    label: '系统诊断',
    description: '检查本地服务、运行环境与故障状态。',
    marker: '诊'
  }
];

/**
 * 设置分组定义，用于把零散的面板按业务维度组织成左侧菜单的分区。
 * 每个分组在菜单中渲染为一个带标题的区块，分组内 panels 顺序即展示顺序。
 */
const SETTINGS_GROUPS: Array<{ label: string; panels: SettingsPanel[] }> = [
  { label: '模型与连接', panels: ['chat'] },
  { label: '语音', panels: ['tts', 'speech_recognition'] },
  { label: '角色外观', panels: ['appearance'] },
  { label: '工具与网络', panels: ['workspace', 'web_search'] },
  { label: '系统', panels: ['diagnostics'] }
];

/**
 * 面板 id -> 图标组件的映射。
 * lucide-react 图标在菜单按钮中作为视觉前缀，帮助用户快速识别各设置分类。
 */
const SETTINGS_PANEL_ICONS: Record<SettingsPanel, LucideIcon> = {
  chat: Bot,
  tts: AudioLines,
  speech_recognition: Mic2,
  workspace: ShieldCheck,
  web_search: Globe2,
  appearance: Palette,
  diagnostics: Activity
};

/**
 * 设置中心主对话框组件。
 * 见文件顶部文档注释对整体职责与 embedded 形态的说明。
 */
export function SettingsDialog(props: SettingsDialogProps) {
  const {
    embedded = false,            // 是否内嵌展示（影响头部、遮罩、ARIA 角色）
    settingsPanel,               // 当前激活的面板 id
    busy,                        // 保存进行中标志，用于禁用提交按钮
    workspacePermissionMode,     // 工作区权限模式；full_access 时给表单加警示样式
    dirtyDomains,                // 已修改但未保存的设置域列表
    setSettingsPanel,            // 切换激活面板的回调
    onClose,                     // 关闭对话框的回调
    onDiscard,                   // 放弃修改的回调（确认弹窗中调用）
    onSubmit                     // 提交保存全部更改的回调
  } = props;
  // 关闭确认弹窗的开关：存在未保存更改时点关闭，会先弹此确认
  const [closeConfirmOpen, setCloseConfirmOpen] = useState(false);
  // 是否存在未保存更改：dirtyDomains 非空即视为脏
  const hasDirtyChanges = dirtyDomains.length > 0;
  /**
   * 请求关闭对话框。
   * 若存在未保存更改，先弹出确认弹窗，避免误操作丢失修改；否则直接关闭。
   */
  const requestClose = useCallback(() => {
    if (hasDirtyChanges) {
      setCloseConfirmOpen(true);
      return;
    }
    onClose();
  }, [hasDirtyChanges, onClose]);
  // 模态无障碍钩子：非内嵌时托管焦点陷阱、Esc 关闭等行为，关闭时触发 requestClose
  const modalRef = useModalAccessibility<HTMLElement>(!embedded, requestClose);

  // 表单根类名：full_access 权限模式下追加警示样式类，提示当前为高权限状态
  const formClassName = [
    'persona-form',
    'settings-form',
    workspacePermissionMode === 'full_access' ? 'full-access-warning' : ''
  ]
    .filter(Boolean)
    .join(' ');
  const panelClassName = [
    'settings-panel',
    settingsPanel === 'chat' ? 'settings-panel-flush' : '',
    settingsPanel !== 'chat' ? 'settings-panel-direct' : ''
  ]
    .filter(Boolean)
    .join(' ');

  return (
    // 模态外壳：embedded 时降级为普通区域，否则作为对话框并拦截背景点击关闭
    <section
      className={`modal-shell settings-modal ${embedded ? 'embedded' : ''}`}
      ref={modalRef}
      tabIndex={-1}
      role={embedded ? 'region' : 'dialog'}
      aria-modal={embedded ? undefined : true}
      aria-label="Muse 设置中心"
      onMouseDown={(event) => {
        // 仅当点击的是外壳本身（即背景遮罩）而非内部内容时才触发关闭
        if (!embedded && event.target === event.currentTarget) requestClose();
      }}
    >
      {/* 整个设置中心是一个表单：底部「保存全部更改」按钮触发 onSubmit */}
      <form className={formClassName} onSubmit={(event) => void onSubmit(event)}>
        {/* 模态头部：仅非内嵌时显示，含标题与「返回剧情」关闭按钮 */}
        {!embedded && (
          <header className="settings-workspace-head">
            <div>
              <small>应用设置</small>
              <strong>设置中心</strong>
              <span>按需要调整 Muse 的对话、角色体验与本地能力。</span>
            </div>
            <button type="button" className="settings-close-button" onClick={requestClose} aria-label="返回剧情">
              <ChevronLeft aria-hidden="true" />
              <span>返回剧情</span>
            </button>
          </header>
        )}

        {/* 主体两栏布局：左侧导航菜单 + 右侧面板内容 */}
        <div className="settings-layout">
          {/* 左侧导航：设置项数量有限，直接展示完整分组，不增加多余搜索层。 */}
          <nav className="settings-menu" aria-label="设置分类">
            <div className="settings-menu-scroll">
              {SETTINGS_GROUPS.map((group) => (
                <section className="settings-menu-group" key={group.label}>
                  <span>{group.label}</span>
                  {group.panels.map((panelId) => {
                    const panel = SETTINGS_PANELS.find((item) => item.id === panelId)!;
                    const Icon = SETTINGS_PANEL_ICONS[panel.id];
                    return (
                      // 面板切换按钮：点击切换 settingsPanel，当前激活项加 active 样式
                      <button
                        type="button"
                        key={panel.id}
                        className={settingsPanel === panel.id ? 'active' : ''}
                        onClick={() => setSettingsPanel(panel.id)}
                      >
                        <Icon aria-hidden="true" />
                        <strong>{panel.label}</strong>
                      </button>
                    );
                  })}
                </section>
              ))}
            </div>
          </nav>

          {/* 右侧面板区：根据当前 settingsPanel 渲染对应子组件，props 透传给各 Panel */}
          <section className={panelClassName}>
            <div className="settings-panel-scroll">
              {settingsPanel === 'chat' && <ChatPanel {...props} />}
              {settingsPanel === 'tts' && <TtsPanel {...props} />}
              {settingsPanel === 'speech_recognition' && <SpeechRecognitionPanel {...props} />}
              {settingsPanel === 'workspace' && <WorkspacePanel {...props} />}
              {settingsPanel === 'web_search' && <WebSearchPanel {...props} />}
              {settingsPanel === 'appearance' && <AppearancePanel {...props} />}
              {settingsPanel === 'diagnostics' && <DiagnosticsPanel {...props} />}
            </div>
          </section>
        </div>

        {/* 底部操作栏：脏数据提示 + 取消/保存按钮 */}
        <footer>
          {/* 脏数据提示：有修改时列出受影响域；无修改时按面板类型给出不同提示 */}
          <span className="settings-dirty-indicator">
            {hasDirtyChanges
              ? `待保存：${dirtyDomains
                  .map((domain) =>
                    ({
                      models: '模型',
                      workspace: '权限',
                      web_search: '联网搜索',
                      appearance: '外观'
                    })[domain]
                  )
                  .join('、')}`
              : settingsPanel === 'chat'
                ? '供应商凭据与模型目录会在操作后立即保存'
                : '所有更改均已保存'}
          </span>
          {/* 取消/返回按钮：chat 面板且无修改时文案为「返回对话」，否则为「取消」 */}
          <button type="button" onClick={requestClose}>
            {settingsPanel === 'chat' && !hasDirtyChanges ? '返回对话' : '取消'}
          </button>
          {/* 保存按钮：仅在有可保存内容时显示；busy 或无修改时禁用 */}
          {(settingsPanel !== 'chat' || hasDirtyChanges) && (
            <button type="submit" disabled={busy || !hasDirtyChanges}>
              保存全部更改
            </button>
          )}
        </footer>
      </form>

      {/* 关闭确认弹窗：存在未保存更改时点关闭/取消触发，避免误丢失修改 */}
      <AlertDialog.Root open={closeConfirmOpen} onOpenChange={setCloseConfirmOpen}>
        <AlertDialog.Portal>
          <AlertDialog.Overlay className="confirm-dialog-overlay" />
          <AlertDialog.Content className="confirm-dialog-content">
            <span className="confirm-dialog-mark danger" aria-hidden="true">
              !
            </span>
            <AlertDialog.Title className="confirm-dialog-title">未保存的更改</AlertDialog.Title>
            <AlertDialog.Description className="confirm-dialog-description">
              您对设置的更改尚未保存，保存前退出将丢失所有修改。
            </AlertDialog.Description>
            <div className="confirm-dialog-actions">
              {/* 继续编辑：仅关闭确认弹窗，回到设置界面 */}
              <AlertDialog.Cancel asChild>
                <button type="button">继续编辑</button>
              </AlertDialog.Cancel>
              {/* 放弃修改并退出：先 onDiscard 清理脏状态，再 onClose 关闭对话框 */}
              <AlertDialog.Action asChild>
                <button
                  type="button"
                  className="danger"
                  onClick={() => {
                    onDiscard();
                    onClose();
                  }}
                >
                  放弃修改并退出
                </button>
              </AlertDialog.Action>
            </div>
          </AlertDialog.Content>
        </AlertDialog.Portal>
      </AlertDialog.Root>
    </section>
  );
}
