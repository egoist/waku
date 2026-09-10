import { describe, expect, test } from 'bun:test';
import type { SkillEntry, SkillsCatalog } from '@waku/client';

import {
  groupSkillsByProject,
  skillEnabled,
  skillInstallDirs,
  skillScopeLabel,
  skillSizeLabel,
  skillSourceLabel,
  skillSourcesLabel,
  skillSummary,
} from './skill-presentation';

function install(overrides: Partial<SkillEntry['installs'][number]> = {}): SkillEntry['installs'][number] {
  return { source: 'shared', dir: '/skills/demo', skillFile: '/skills/demo/SKILL.md', enabled: true, ...overrides };
}

function skill(overrides: Partial<SkillEntry> = {}): SkillEntry {
  return {
    name: 'demo',
    description: 'Does a thing',
    scope: 'user',
    project: null,
    installs: [install()],
    enabled: true,
    allowedTools: null,
    body: '',
    supportingFiles: 0,
    totalBytes: 2_048,
    modifiedAt: null,
    duplicates: 0,
    rowKey: 1,
    ...overrides,
  };
}

describe('skill presentation', () => {
  test('names the source of an install', () => {
    expect(skillSourceLabel('shared')).toBe('Shared');
    expect(skillSourceLabel({ provider: 'codex' })).toBe('Codex');
    expect(skillSourceLabel({ provider: 'claude' })).toBe('Claude');
  });

  test('lists each distinct source once', () => {
    const entry = skill({
      installs: [
        install(),
        install({ source: { provider: 'codex' }, dir: '/b' }),
        install({ source: { provider: 'codex' }, dir: '/c' }),
      ],
    });
    expect(skillSourcesLabel(entry)).toBe('Shared, Codex');
  });

  test('reports a skill as on when any install is on', () => {
    expect(skillEnabled(skill())).toBe(true);
    expect(skillEnabled(skill({
      installs: [install({ enabled: false }), install({ enabled: true, dir: '/b' })],
    }))).toBe(true);
    expect(skillEnabled(skill({ installs: [install({ enabled: false })] }))).toBe(false);
  });

  test('collects the directories a toggle has to write', () => {
    const entry = skill({ installs: [install(), install({ dir: '/other' })] });
    expect(skillInstallDirs(entry)).toEqual(['/skills/demo', '/other']);
  });

  test('scopes a skill to its project or the user', () => {
    expect(skillScopeLabel(skill())).toBe('User');
    expect(skillScopeLabel(skill({ project: 'Waku' }))).toBe('Project · Waku');
  });

  test('sizes a directory in kB or MB', () => {
    expect(skillSizeLabel(skill({ totalBytes: 512 }))).toBe('1 kB');
    expect(skillSizeLabel(skill({ totalBytes: 2_048 }))).toBe('2 kB');
    expect(skillSizeLabel(skill({ totalBytes: 3 * 1024 * 1024 }))).toBe('3.0 MB');
  });

  test('groups by project, keeping user skills together', () => {
    const catalog: SkillsCatalog = {
      skills: [
        skill({ name: 'a', project: 'Waku' }),
        skill({ name: 'b' }),
        skill({ name: 'c', project: 'Waku' }),
      ],
    };
    const groups = groupSkillsByProject(catalog.skills);
    expect(groups.map((group) => [group.project, group.skills.map((entry) => entry.name)])).toEqual([
      ['Waku', ['a', 'c']],
      [null, ['b']],
    ]);
  });

  test('summarizes the catalog with its disabled count', () => {
    expect(skillSummary([skill()])).toBe('1 skill');
    expect(skillSummary([skill(), skill({ enabled: false }), skill({ enabled: false })])).toBe(
      '3 skills · 2 disabled',
    );
    expect(skillSummary([])).toBe('0 skills');
  });
});
