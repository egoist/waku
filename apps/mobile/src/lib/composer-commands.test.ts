import { describe, expect, test } from 'bun:test';
import type { ReportedCommand, SlashCommand } from '@waku/client';

import {
  detectComposerTrigger,
  expandCommandTemplate,
  filterComposerCommands,
  mergeComposerCommands,
  replaceComposerTrigger,
  resolvedComposerSubmission,
} from './composer-commands';

function command(
  name: string,
  scope: SlashCommand['scope'] = 'Builtin',
  template: string | null = null,
): SlashCommand {
  return { name, description: '', scope, argument_hint: null, template };
}

function reported(name: string, description = ''): ReportedCommand {
  return { name, description };
}

describe('composer command triggers', () => {
  test('triggers on a slash at the start of the caret line', () => {
    expect(detectComposerTrigger('/rev', 4)).toEqual({ query: 'rev', start: 0, end: 4 });
    expect(detectComposerTrigger('intro\n/rev', 10)).toEqual({ query: 'rev', start: 6, end: 10 });
  });

  test('stops once an argument is typed', () => {
    expect(detectComposerTrigger('/review this', 12)).toBeNull();
  });

  test('ignores a slash that is not at the line start', () => {
    expect(detectComposerTrigger('look at a/b', 11)).toBeNull();
    expect(detectComposerTrigger('', 0)).toBeNull();
  });

  test('clamps a caret past the end of the text', () => {
    expect(detectComposerTrigger('/rev', 99)).toEqual({ query: 'rev', start: 0, end: 4 });
  });
});

describe('command filtering', () => {
  const commands = [
    command('review', 'Project'),
    command('release', 'User'),
    command('compact', 'Builtin'),
    command('deploy', 'Skill'),
  ];

  test('ranks prefix hits above interior ones', () => {
    // Both start with "re", so picker order (Project before User) decides.
    expect(filterComposerCommands(commands, 're').map((item) => item.name)).toEqual([
      'review',
      'release',
    ]);
  });

  test('ranks a prefix hit above an interior one', () => {
    const mixed = [command('release', 'User'), command('review', 'Project')];
    expect(filterComposerCommands(mixed, 'rev').map((item) => item.name)).toEqual(['review']);
  });

  test('falls back to interior matches everywhere in the name', () => {
    expect(filterComposerCommands(commands, 'ploy').map((item) => item.name)).toEqual(['deploy']);
  });

  test('offers everything, in picker order, before any query', () => {
    const merged = mergeComposerCommands(commands, []);
    expect(filterComposerCommands(merged, '').map((item) => item.name)).toEqual([
      'compact',
      'review',
      'release',
      'deploy',
    ]);
  });

  test('caps the list', () => {
    expect(filterComposerCommands(commands, '', 2)).toHaveLength(2);
  });
});

describe('merging reported commands', () => {
  test('orders by scope then name', () => {
    const merged = mergeComposerCommands(
      [command('deploy', 'Skill'), command('lint', 'Project'), command('review', 'Builtin')],
      [reported('compact')],
    );
    expect(merged.map((item) => [item.scope, item.name])).toEqual([
      ['Builtin', 'compact'],
      ['Builtin', 'review'],
      ['Project', 'lint'],
      ['Skill', 'deploy'],
    ]);
  });

  test('fills a blank description but never redefines the command', () => {
    const [known] = mergeComposerCommands(
      [{ ...command('compact'), description: '' }],
      [reported('compact', 'Free up context')],
    );
    expect(known?.description).toBe('Free up context');

    // The project owns /deploy, so the report only labels it.
    const project = command('deploy', 'Project', 'ship it');
    const [kept] = mergeComposerCommands([project], [reported('deploy', 'CLI deploy')]);
    expect(kept?.template).toBe('ship it');
    expect(kept?.scope).toBe('Project');
    expect(kept?.description).toBe('CLI deploy');
  });
});

describe('inserting a command', () => {
  test('replaces the trigger and leaves the caret after it', () => {
    const trigger = detectComposerTrigger('/rev', 4)!;
    expect(replaceComposerTrigger('/rev', trigger, command('review'))).toEqual({
      text: '/review ',
      cursor: 8,
    });
    expect(replaceComposerTrigger('look /rev', { query: 'rev', start: 5, end: 9 }, command('review')))
      .toEqual({ text: 'look /review ', cursor: 13 });
  });
});

describe('template expansion', () => {
  test('substitutes ARGUMENTS and positional placeholders', () => {
    expect(expandCommandTemplate('Review $ARGUMENTS now', 'src/api')).toBe('Review src/api now');
    expect(expandCommandTemplate('Compare $1 and $2', 'a b')).toBe('Compare a and b');
    expect(expandCommandTemplate('Fix $@', 'login bug')).toBe('Fix login bug');
  });

  test('appends unconsumed arguments', () => {
    expect(expandCommandTemplate('Review this', 'extra detail')).toBe('Review this\n\nextra detail');
  });

  test('leaves a dollar sign that is not a placeholder alone', () => {
    expect(expandCommandTemplate('Costs $x', '')).toBe('Costs $x');
  });

  test('drops a positional placeholder with no matching argument', () => {
    expect(expandCommandTemplate('Compare $1', '')).toBe('Compare ');
  });
});

describe('resolving a submission', () => {
  test('passes built-in commands through untouched', () => {
    expect(resolvedComposerSubmission('claude', '/compact', [command('compact')])).toBeNull();
  });

  test('expands a template command to its body', () => {
    const commands = [command('review', 'Project', 'Review $ARGUMENTS carefully')];
    expect(resolvedComposerSubmission('claude', '/review src/api', commands))
      .toBe('Review src/api carefully');
  });

  test('turns a skill into the provider native invocation', () => {
    const commands = [command('to-spec', 'Skill')];
    expect(resolvedComposerSubmission('codex', '/to-spec now', commands)).toBe('$to-spec now');
    expect(resolvedComposerSubmission('fx', '/to-spec now', commands)).toBe('$to-spec now');
    expect(resolvedComposerSubmission('pi', '/to-spec now', commands)).toBe('/skill:to-spec now');
    // Claude has no native skill syntax, so the text goes through as typed.
    expect(resolvedComposerSubmission('claude', '/to-spec now', commands)).toBeNull();
  });

  test('ignores text that is not a command', () => {
    expect(resolvedComposerSubmission('codex', 'plain prompt', [])).toBeNull();
    expect(resolvedComposerSubmission('codex', '/unknown', [command('known')])).toBeNull();
  });
});
