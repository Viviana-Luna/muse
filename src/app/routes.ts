export type MuseSection =
  | 'chat'
  | 'sessions'
  | 'roles'
  | 'skills'
  | 'mcp'
  | 'settings';

export interface MuseRoute {
  section: MuseSection;
  name?: string;
}

const ROUTE_SECTIONS = new Set<MuseSection>([
  'chat',
  'sessions',
  'roles',
  'skills',
  'mcp',
  'settings'
]);

export function migrateLegacyMuseUrl() {
  if (typeof window === 'undefined') return;
  if (window.location.hash.startsWith('#/')) return;
  const url = new URL(window.location.href);
  const legacy = url.searchParams.get('view');
  const section: MuseSection = legacy === 'roles' || legacy === 'settings' ? legacy : 'chat';
  url.searchParams.delete('view');
  url.hash = `/${section}`;
  window.history.replaceState({}, '', url);
}

export function readMuseRoute(): MuseRoute {
  if (typeof window === 'undefined') return { section: 'chat' };
  const segments = window.location.hash
    .replace(/^#\/?/u, '')
    .split('/')
    .filter(Boolean)
    .map((segment) => decodeURIComponent(segment));
  const candidate = segments[0] as MuseSection | undefined;
  const section = candidate && ROUTE_SECTIONS.has(candidate) ? candidate : 'chat';
  return { section, name: segments[1] || undefined };
}

export function writeMuseRoute(route: MuseRoute, replace = false) {
  const suffix = route.name ? `/${encodeURIComponent(route.name)}` : '';
  const hash = `#/${route.section}${suffix}`;
  if (replace) window.history.replaceState({}, '', hash);
  else window.location.hash = hash;
}
