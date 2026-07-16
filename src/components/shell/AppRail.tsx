import {
  Bot,
  Boxes,
  Cable,
  MessagesSquare,
  Settings,
  Sparkles,
  UserRound
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import type { MuseSection } from '@/app/routes';

interface RailItem {
  section: MuseSection;
  label: string;
  icon: LucideIcon;
}

const PRIMARY_ITEMS: RailItem[] = [
  { section: 'chat', label: '对话', icon: MessagesSquare },
  { section: 'sessions', label: '会话', icon: Bot },
  { section: 'roles', label: '角色', icon: UserRound },
  { section: 'skills', label: 'Skill', icon: Sparkles },
  { section: 'mcp', label: 'MCP', icon: Cable }
];

export function AppRail({
  active,
  onNavigate
}: {
  active: MuseSection;
  onNavigate: (section: MuseSection) => void;
}) {
  return (
    <aside className="app-rail" aria-label="Muse 功能栏">
      <button
        type="button"
        className="app-rail-brand"
        aria-label="返回对话"
        onClick={() => onNavigate('chat')}
      >
        <img src="/assets/muse-logo.png" alt="" aria-hidden="true" />
        <span>Muse</span>
      </button>
      <nav aria-label="主要功能">
        {PRIMARY_ITEMS.map((item) => {
          const Icon = item.icon;
          return (
            <button
              type="button"
              key={item.section}
              className={active === item.section ? 'active' : ''}
              aria-label={item.label}
              aria-current={active === item.section ? 'page' : undefined}
              onClick={() => onNavigate(item.section)}
            >
              <Icon aria-hidden="true" />
              <span>{item.label}</span>
            </button>
          );
        })}
      </nav>
      <div className="app-rail-spacer" />
      <button
        type="button"
        className={active === 'settings' ? 'active' : ''}
        aria-label="设置"
        aria-current={active === 'settings' ? 'page' : undefined}
        onClick={() => onNavigate('settings')}
      >
        <Settings aria-hidden="true" />
        <span>设置</span>
      </button>
      <span className="app-rail-build" title="本地 Agent 工作台">
        <Boxes aria-hidden="true" />
      </span>
    </aside>
  );
}
