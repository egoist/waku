import type { SkillEntry, SkillInstall, SkillSource } from '@waku/client';

import { providerLabel } from './session-presentation';

/** Where a skill was installed from: Waku's shared library, or one
 * provider's own directory. */
export function skillSourceLabel(source: SkillSource): string {
  return typeof source === 'string' ? 'Shared' : providerLabel(source.provider);
}

export function skillSourcesLabel(entry: SkillEntry): string {
  if (!entry.installs.length) return skillSourceLabel('shared');
  const seen: string[] = [];
  for (const install of entry.installs) {
    const label = skillSourceLabel(install.source);
    if (!seen.includes(label)) seen.push(label);
  }
  return seen.join(', ');
}

/** A skill disabled in one install but not another is partly on — which the
 * toggle reports as on, since pressing it then turns everything off. */
export function skillEnabled(entry: SkillEntry): boolean {
  return entry.installs.some((install) => install.enabled);
}

export function skillInstallDirs(entry: SkillEntry): string[] {
  return entry.installs.map((install) => install.dir);
}

export function skillScopeLabel(entry: SkillEntry): string {
  return entry.project ? `Project · ${entry.project}` : 'User';
}

/** Byte counts are for a directory, not a file, so they round to kB. */
export function skillSizeLabel(entry: SkillEntry): string {
  const kb = entry.totalBytes / 1024;
  return kb < 1024 ? `${Math.max(1, Math.round(kb))} kB` : `${(kb / 1024).toFixed(1)} MB`;
}

/** Skills are listed newest-first within a project group, matching desktop. */
export function groupSkillsByProject(
  skills: SkillEntry[],
): Array<{ project: string | null; skills: SkillEntry[] }> {
  const groups = new Map<string, { project: string | null; skills: SkillEntry[] }>();
  for (const skill of skills) {
    const key = skill.project ?? '';
    const group = groups.get(key) ?? { project: skill.project, skills: [] };
    group.skills.push(skill);
    groups.set(key, group);
  }
  return [...groups.values()];
}

export function skillSummary(skills: SkillEntry[]): string {
  const disabled = skills.filter((entry) => !entry.enabled).length;
  const count = `${skills.length} skill${skills.length === 1 ? '' : 's'}`;
  return disabled ? `${count} · ${disabled} disabled` : count;
}

export type { SkillInstall };
