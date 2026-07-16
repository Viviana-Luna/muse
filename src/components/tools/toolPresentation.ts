import {
  FilePenLine,
  Globe2,
  ShieldAlert,
  Terminal
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';

export interface ToolPresentation {
  icon: LucideIcon;
  shortImpact: string;
}

export function toolActionLabel(toolName?: string) {
  const labels: Record<string, string> = {
    todo_write: '更新任务',
    enter_plan_mode: '进入计划',
    exit_plan_mode: '计划确认',
    send_user_message: '阶段简报',
    brief: '阶段简报',
    load_skill: '载入技能',
    use_skill: '载入技能',
    skill: '载入技能',
    agent: '登记子任务',
    task_stop: '停止任务',
    file_read: '读取文件',
    file_list: '列出目录',
    file_search: '搜索文件',
    file_write: '写入文件',
    file_edit: '编辑文件',
    command_run: '执行命令',
    web_search: '联网搜索',
    web_fetch: '抓取网页',
    persona_switch: '切换角色',
    tts_speak: '语音朗读',
    ask_user_question: '用户提问'
  };
  return labels[toolName || ''] || toolName || '工具调用';
}

export function toolPresentation(toolName?: string): ToolPresentation {
  switch (toolName) {
    case 'web_search':
    case 'web_fetch':
      return {
        icon: Globe2,
        shortImpact: '会向外部服务发送本次搜索词。'
      };
    case 'file_write':
    case 'file_edit':
      return {
        icon: FilePenLine,
        shortImpact: '会修改本机工作区中的文件。'
      };
    case 'command_run':
      return {
        icon: Terminal,
        shortImpact: '会在本机执行命令。'
      };
    default:
      return {
        icon: ShieldAlert,
        shortImpact: '这项行动可能产生可见影响。'
      };
  }
}
