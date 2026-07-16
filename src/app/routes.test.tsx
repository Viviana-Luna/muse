import { beforeEach, describe, expect, it } from 'vitest';
import { migrateLegacyMuseUrl, readMuseRoute, writeMuseRoute } from './routes';

describe('Muse Hash 路由', () => {
  beforeEach(() => window.history.replaceState({}, '', '/'));

  it('把旧 view 查询参数无损转换为 Hash 路由', () => {
    window.history.replaceState({}, '', '/?view=roles');
    migrateLegacyMuseUrl();
    expect(window.location.search).toBe('');
    expect(window.location.hash).toBe('#/roles');
    expect(readMuseRoute()).toEqual({ section: 'roles', name: undefined });
  });

  it('保留 Skill 详情名称并安全编码', () => {
    writeMuseRoute({ section: 'skills', name: 'daily-notes' });
    expect(window.location.hash).toBe('#/skills/daily-notes');
    expect(readMuseRoute()).toEqual({ section: 'skills', name: 'daily-notes' });
  });
});
