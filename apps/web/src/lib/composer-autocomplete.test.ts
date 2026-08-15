import { describe, expect, test } from 'bun:test'
import type { FileEntry, ReportedCommand, SlashCommand } from '@waku/client'
import {
  composerAutocompleteRows,
  detectComposerTrigger,
  expandCommandTemplate,
  expandedComposerSubmission,
  mergeComposerCommands,
  replaceComposerTrigger,
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
    const discovered = [command('review', 'Project', 'Review changes', 'Review $ARGUMENTS')]
    const reported: ReportedCommand[] = [
      { name: 'review', description: 'Provider review' },
      { name: 'compact', description: 'Compact context' },
    ]
    expect(mergeComposerCommands(discovered, reported)).toEqual([
      discovered[0],
      command('compact', 'Builtin', 'Compact context', null),
    ])
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
    expect(expandedComposerSubmission('/review src', commands)).toBe('Review src')
    expect(expandedComposerSubmission('/unknown src', commands)).toBeNull()
    expect(expandedComposerSubmission('please /review src', commands)).toBeNull()
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
