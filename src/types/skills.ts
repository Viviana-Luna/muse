export interface SkillSummary {
  name: string;
  description: string;
  enabled: boolean;
  revision: string;
  updated_at: string;
}

export interface SkillRecord extends SkillSummary {
  content: string;
}

export interface SkillCatalogDiagnostic {
  name: string;
  code: string;
  message: string;
}

export interface SkillCatalogSnapshot {
  skills: SkillSummary[];
  diagnostics: SkillCatalogDiagnostic[];
  omitted_diagnostic_count: number;
}

export interface SkillDraft {
  name: string;
  description: string;
  content: string;
  enabled: boolean;
}

export interface SkillUpdate extends SkillDraft {
  revision: string;
}

export const SKILL_NAME_ERROR =
  'Skill 名称必须为 1-64 个小写字母、数字或单连字符组合，格式如 `git-release`。';

export function validateSkillName(value: string): string | null {
  return /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value.trim()) && value.trim().length <= 64
    ? null
    : SKILL_NAME_ERROR;
}
