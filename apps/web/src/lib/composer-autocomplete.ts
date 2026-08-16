import type {
  FileEntry,
  ProviderKind,
  ProviderModelOption,
  ReportedCommand,
  SlashCommand,
} from '@waku/client'
import { fuzzyScore } from './palette-search'

export const COMPOSER_AUTOCOMPLETE_CAP = 64

export type ComposerTriggerKind = 'command' | 'file'

export interface ComposerTrigger {
  kind: ComposerTriggerKind
  query: string
  start: number
  end: number
}

export type ComposerAutocompleteRow =
  | { kind: 'command'; command: SlashCommand }
  | { kind: 'file'; file: FileEntry }

export function detectComposerTrigger(text: string, cursor: number): ComposerTrigger | null {
  const end = Math.max(0, Math.min(cursor, text.length))
  const lineStart = text.lastIndexOf('\n', end - 1) + 1
  const linePrefix = text.slice(lineStart, end)
  if (linePrefix.startsWith('/')) {
    const query = linePrefix.slice(1)
    if (/\s/u.test(query)) return null
    return { kind: 'command', query, start: lineStart, end }
  }

  let tokenStart = end
  while (tokenStart > 0 && !/\s/u.test(text[tokenStart - 1]!)) tokenStart -= 1
  const token = text.slice(tokenStart, end)
  if (!token.startsWith('@')) return null
  return { kind: 'file', query: token.slice(1), start: tokenStart, end }
}

export function mergeComposerCommands(
  discovered: SlashCommand[],
  reported: ReportedCommand[],
): SlashCommand[] {
  const merged = discovered.map((command) => ({ ...command }))
  for (const report of reported) {
    const known = merged.find((command) => command.name === report.name)
    if (known) {
      if (!known.description) known.description = report.description ?? ''
      continue
    }
    merged.push({
      name: report.name,
      description: report.description ?? '',
      scope: 'Builtin',
      argument_hint: null,
      template: null,
    })
  }
  return merged.sort((left, right) => {
    const byScope = commandScopeRank(left.scope) - commandScopeRank(right.scope)
    return byScope || left.name.localeCompare(right.name)
  })
}

export function isFastModeToggleSubmission(
  provider: ProviderKind,
  prompt: string,
  commands: SlashCommand[],
): boolean {
  return provider === 'codex'
    && prompt.trim() === '/fast'
    && commands.some((command) => command.name === 'fast'
      && command.scope === 'Builtin'
      && command.template === null)
}

export function toggledFastServiceTier(
  current: string | null | undefined,
  serviceTiers: ProviderModelOption[],
): string | null {
  const fast = serviceTiers.find((tier) => ['fast', 'priority'].includes(tier.id)
    || tier.label.toLocaleLowerCase() === 'fast')
  if (!fast) return null
  return current === fast.id ? 'default' : fast.id
}

export function composerAutocompleteRows(
  trigger: ComposerTrigger,
  commands: SlashCommand[],
  files: FileEntry[],
  cap = COMPOSER_AUTOCOMPLETE_CAP,
): ComposerAutocompleteRow[] {
  const source = trigger.kind === 'command'
    ? commands.map((command) => ({
        row: { kind: 'command' as const, command },
        candidate: command.name,
      }))
    : files.map((file) => ({
        row: { kind: 'file' as const, file },
        candidate: file.path,
      }))
  if (!trigger.query.trim()) return source.slice(0, cap).map(({ row }) => row)

  return source
    .map(({ row, candidate }, index) => ({
      row,
      index,
      score: fuzzyScore(trigger.query, candidate),
    }))
    .filter((item): item is typeof item & { score: number } => item.score !== null)
    .sort((left, right) => right.score - left.score || left.index - right.index)
    .slice(0, cap)
    .map(({ row }) => row)
}

export function replaceComposerTrigger(
  text: string,
  trigger: ComposerTrigger,
  row: ComposerAutocompleteRow,
): { text: string; cursor: number } {
  const insert = row.kind === 'command'
    ? `/${row.command.name} `
    : `@${row.file.path} `
  const next = `${text.slice(0, trigger.start)}${insert}${text.slice(trigger.end)}`
  return { text: next, cursor: trigger.start + insert.length }
}

export function expandCommandTemplate(template: string, args: string): string {
  const positional = args.split(/\s+/u).filter(Boolean)
  let expanded = ''
  let consumedArgs = false
  let rest = template
  while (true) {
    const index = rest.indexOf('$')
    if (index < 0) break
    expanded += rest.slice(0, index)
    const after = rest.slice(index + 1)
    if (after.startsWith('ARGUMENTS')) {
      expanded += args
      consumedArgs = true
      rest = after.slice('ARGUMENTS'.length)
    } else if (after.startsWith('@')) {
      expanded += args
      consumedArgs = true
      rest = after.slice(1)
    } else if (/^[1-9]/u.test(after)) {
      expanded += positional[Number(after[0]) - 1] ?? ''
      consumedArgs = true
      rest = after.slice(1)
    } else {
      expanded += '$'
      rest = after
    }
  }
  expanded += rest
  if (!consumedArgs && args) expanded += `\n\n${args}`
  return expanded
}

export function expandedComposerSubmission(
  prompt: string,
  commands: SlashCommand[],
): string | null {
  if (!prompt.startsWith('/')) return null
  const invocation = prompt.slice(1)
  const whitespace = invocation.search(/\s/u)
  const name = whitespace < 0 ? invocation : invocation.slice(0, whitespace)
  const args = whitespace < 0 ? '' : invocation.slice(whitespace).trim()
  const command = commands.find((item) => item.name === name && item.template !== null)
  return command?.template === null || command?.template === undefined
    ? null
    : expandCommandTemplate(command.template, args)
}

function commandScopeRank(scope: SlashCommand['scope']): number {
  return ['Builtin', 'Project', 'User', 'Skill'].indexOf(scope)
}
