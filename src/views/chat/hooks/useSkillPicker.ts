import { useCallback, useEffect, useRef, useState } from 'react';

import { listRuntimeSkills } from '@/api';
import type { RuntimeSkillSummary } from '@/types';

export function useSkillPicker(personaId: string | null) {
  const [open, setOpen] = useState(false);
  const [skills, setSkills] = useState<RuntimeSkillSummary[]>([]);
  const [selectedSkill, setSelectedSkill] = useState<RuntimeSkillSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [omittedSkillCount, setOmittedSkillCount] = useState(0);
  const requestRevisionRef = useRef(0);

  const refresh = useCallback(async () => {
    if (!personaId) return;
    const requestRevision = requestRevisionRef.current + 1;
    requestRevisionRef.current = requestRevision;
    setLoading(true);
    setError(null);
    try {
      const snapshot = await listRuntimeSkills();
      if (requestRevisionRef.current !== requestRevision) return;
      setSkills(snapshot.skills);
      setOmittedSkillCount(snapshot.omitted_skill_count);
      setSelectedSkill((current) =>
        current
          ? snapshot.skills.find(
              (skill) => skill.name === current.name && skill.revision === current.revision
            ) ?? null
          : null
      );
    } catch (reason) {
      if (requestRevisionRef.current !== requestRevision) return;
      setError(reason instanceof Error ? reason.message : '无法读取当前可用 Skill。');
    } finally {
      if (requestRevisionRef.current === requestRevision) setLoading(false);
    }
  }, [personaId]);

  const openPicker = useCallback(() => {
    if (!personaId) return;
    setOpen(true);
    void refresh();
  }, [personaId, refresh]);

  const closePicker = useCallback(() => setOpen(false), []);
  const selectSkill = useCallback((skill: RuntimeSkillSummary) => {
    setSelectedSkill(skill);
    setOpen(false);
  }, []);
  const clearSelection = useCallback(() => setSelectedSkill(null), []);

  useEffect(() => {
    requestRevisionRef.current += 1;
    setOpen(false);
    setSkills([]);
    setSelectedSkill(null);
    setLoading(false);
    setError(null);
    setOmittedSkillCount(0);
  }, [personaId]);

  return {
    open,
    skills,
    selectedSkill,
    loading,
    error,
    omittedSkillCount,
    openPicker,
    closePicker,
    selectSkill,
    clearSelection,
    refresh
  };
}

export type SkillPickerController = ReturnType<typeof useSkillPicker>;
