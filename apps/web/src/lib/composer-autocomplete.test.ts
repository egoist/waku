import { describe, expect, test } from 'bun:test'
import type { FileEntry, ReportedCommand, SlashCommand } from '@waku/client'
import {
  composerAutocompleteRows,
  detectComposerTrigger,
  expandCommandTemplate,
  expandedComposerSubmission,
  isFastModeToggleSubmission,
  isResumeSubmission,
  mergeComposerCommands,
  parseGoalSubmission,
  replaceComposerTrigger,
  toggledFastServiceTier,
} from './composer-autocomplete'

describe('composer autocomplete', () => {
  test('detects slash commands only at the start of the current line', () => {
    expect(detectComposerTrigger('intro\n/rev', 10)).toEqual({
      kind: 'command',
      query: 'rev',
      start: 6,
      end: 10,
    })
    expect(detectComposerTrigger('intro /rev', 10)).toBeNull()
    expect(detectComposerTrigger('/review now', 11)).toBeNull()
  })

  test('detects the current whitespace-delimited file mention', () => {
    expect(detectComposerTrigger('look at @src/app', 16)).toEqual({
      kind: 'file',
      query: 'src/app',
      start: 8,
      end: 16,
    })
    expect(detectComposerTrigger('email@example.com', 17)).toBeNull()
  })

  test('replaces only the active token and leaves the caret after a trailing space', () => {
    const trigger = detectComposerTrigger('read @app then', 9)!
    expect(replaceComposerTrigger('read @app then', trigger, {
      kind: 'file',
      file: { path: 'src/app.ts', is_dir: false },
    })).toEqual({ text: 'read @src/app.ts  then', cursor: 17 })
  })

  test('merges live provider commands without losing discovered templates', () => {
    const discovered = [
      command('review', 'Project', 'Review changes', 'Review $ARGUMENTS'),
      command('deploy', 'Skill', 'Deploy the app', null),
      command('resume', 'Waku', 'Resume a session', null),
    ]
    const reported: ReportedCommand[] = [
      { name: 'review', description: 'Provider review' },
      { name: 'compact', description: 'Compact context' },
      { name: 'resume', description: 'Provider resume' },
    ]
    expect(mergeComposerCommands(discovered, reported)).toEqual([
      command('compact', 'Builtin', 'Compact context', null),
      discovered[2],
      discovered[0],
      discovered[1],
    ])
  })

  test('recognizes only the resolved Codex fast-mode command', () => {
    const builtin = command('fast', 'Builtin', 'Toggle fast mode', null)
    expect(isFastModeToggleSubmission('codex', '/fast ', [builtin])).toBe(true)
    expect(isFastModeToggleSubmission('claude', '/fast', [builtin])).toBe(false)
    expect(isFastModeToggleSubmission('codex', '/fast now', [builtin])).toBe(false)
    expect(isFastModeToggleSubmission('codex', '/fast', [
      command('fast', 'Project', 'Project fast command', 'Run fast'),
    ])).toBe(false)
  })

  test('recognizes Resume as an exact provider-neutral local command', () => {
    expect(isResumeSubmission('/resume')).toBe(true)
    expect(isResumeSubmission('  /resume  ')).toBe(true)
    expect(isResumeSubmission('/resume latest')).toBe(false)
    expect(isResumeSubmission('please /resume')).toBe(false)
  })

  test('toggles the concrete Fast service-tier ID reported by the model', () => {
    const tiers = [{ id: 'priority', label: 'Fast' }]
    expect(toggledFastServiceTier('default', tiers)).toBe('priority')
    expect(toggledFastServiceTier('priority', tiers)).toBe('default')
    expect(toggledFastServiceTier(null, [])).toBeNull()
  })

  test('filters by fuzzy path and caps the result count', () => {
    const files: FileEntry[] = Array.from({ length: 100 }, (_, index) => ({
      path: `src/component-${index}.tsx`,
      is_dir: false,
    }))
    const trigger = { kind: 'file' as const, query: 'cmp1', start: 0, end: 5 }
    const rows = composerAutocompleteRows(trigger, [], files, 3)
    expect(rows).toHaveLength(3)
    expect(rows.every((row) => row.kind === 'file')).toBe(true)
  })
})

describe('slash command templates', () => {
  test('expands all-argument and positional placeholders', () => {
    expect(expandCommandTemplate('All: $ARGUMENTS / $@ / $1 / $3', 'one two three'))
      .toBe('All: one two three / one two three / one / three')
  })

  test('appends arguments when a template has no placeholder', () => {
    expect(expandCommandTemplate('Review this project', 'carefully'))
      .toBe('Review this project\n\ncarefully')
  })

  test('expands only a known command with a template', () => {
    const commands = [command('review', 'Project', '', 'Review $ARGUMENTS')]
    expect(expandedComposerSubmission('openCode', '/review src', commands)).toBe('Review src')
    expect(expandedComposerSubmission('openCode', '/unknown src', commands)).toBeNull()
    expect(expandedComposerSubmission('openCode', 'please /review src', commands)).toBeNull()
  })

  test('uses each provider native skill invocation', () => {
    const commands = [command('deploy', 'Skill', '', null)]
    expect(expandedComposerSubmission('fx', '/deploy production', commands))
      .toBe('$deploy production')
    expect(expandedComposerSubmission('pi', '/deploy production', commands))
      .toBe('/skill:deploy production')
  })
})

function command(
  name: string,
  scope: SlashCommand['scope'],
  description: string,
  template: string | null,
): SlashCommand {
  return { name, scope, description, template, argument_hint: null }
}

describe('parseGoalSubmission', () => {
  const builtin: SlashCommand = {
    name: 'goal',
    description: '',
    scope: 'Builtin',
    argument_hint: null,
    template: null,
  }

  test('parses each goal intent', () => {
    const parse = (prompt: string) => parseGoalSubmission('codex', prompt, [builtin])
    expect(parse('/goal')).toEqual({ kind: 'show' })
    expect(parse('/goal ')).toEqual({ kind: 'show' })
    expect(parse('/goal edit')).toEqual({ kind: 'edit' })
    expect(parse('/goal pause')).toEqual({ kind: 'pause' })
    expect(parse('/goal resume')).toEqual({ kind: 'resume' })
    expect(parse('/goal clear')).toEqual({ kind: 'clear' })
    expect(parse('/goal improve benchmark coverage')).toEqual({
      kind: 'set',
      objective: 'improve benchmark coverage',
    })
    expect(parse('/goals')).toBeNull()
    expect(parse('ship /goal')).toBeNull()
  })

  test('is codex-only and respects command overrides', () => {
    expect(parseGoalSubmission('claude', '/goal', [builtin])).toBeNull()
    const projectOwned: SlashCommand = {
      ...builtin,
      scope: 'Project',
      template: 'do project things',
    }
    expect(parseGoalSubmission('codex', '/goal', [projectOwned])).toBeNull()
  })
})
