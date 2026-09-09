import type {
  ProviderKind,
  ReportedCommand,
  SlashCommand,
} from '@waku/client';

export interface ComposerTrigger {
  query: string;
  start: number;
  end: number;
}

/**
 * The slash-command trigger at the caret, or null. A command only triggers at
 * the start of the caret's line and stops at the first whitespace, the same
 * rule the desktop picker uses, so typing an argument dismisses the list.
 */
export function detectComposerTrigger(text: string, cursor: number): ComposerTrigger | null {
  const end = Math.max(0, Math.min(cursor, text.length));
  const lineStart = text.lastIndexOf('\n', end - 1) + 1;
  const linePrefix = text.slice(lineStart, end);
  if (!linePrefix.startsWith('/')) return null;
  const query = linePrefix.slice(1);
  if (/\s/u.test(query)) return null;
  return { query, start: lineStart, end };
}

export const COMPOSER_COMMAND_CAP = 8;

/** Commands matching the query, in picker order. Prefix hits rank above
 * interior ones; the merged order (scope, then name) decides the rest. */
export function filterComposerCommands(
  commands: SlashCommand[],
  query: string,
  cap = COMPOSER_COMMAND_CAP,
): SlashCommand[] {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) return commands.slice(0, cap);
  return commands
    .map((command, index) => ({
      command,
      rank: command.name.toLocaleLowerCase().startsWith(normalized) ? 0 : 1,
      matches: command.name.toLocaleLowerCase().includes(normalized),
      index,
    }))
    .filter((candidate) => candidate.matches)
    // Stable sort: equal ranks keep the merged order rather than re-sorting
    // by name and losing scope precedence.
    .sort((left, right) => left.rank - right.rank || left.index - right.index)
    .slice(0, cap)
    .map((candidate) => candidate.command);
}

/** Provider-reported commands (what the CLI says it understands) folded into
 * discovered ones (what the project, user, and skills define). A report only
 * fills in a missing description — it never overrides a real definition,
 * because a project command may deliberately own the same name. */
export function mergeComposerCommands(
  discovered: SlashCommand[],
  reported: ReportedCommand[],
): SlashCommand[] {
  const merged = discovered.map((command) => ({ ...command }));
  for (const report of reported) {
    const known = merged.find((command) => command.name === report.name);
    if (known) {
      if (!known.description) known.description = report.description ?? '';
      continue;
    }
    merged.push({
      name: report.name,
      description: report.description ?? '',
      scope: 'Builtin',
      argument_hint: null,
      template: null,
    });
  }
  return merged.sort((left, right) => (
    commandScopeRank(left.scope) - commandScopeRank(right.scope)
    || left.name.localeCompare(right.name)
  ));
}

/** Replace the trigger with the chosen command, leaving the caret after the
 * trailing space so arguments can be typed straight away. */
export function replaceComposerTrigger(
  text: string,
  trigger: ComposerTrigger,
  command: SlashCommand,
): { text: string; cursor: number } {
  const insert = `/${command.name} `;
  return {
    text: `${text.slice(0, trigger.start)}${insert}${text.slice(trigger.end)}`,
    cursor: trigger.start + insert.length,
  };
}

export function expandCommandTemplate(template: string, args: string): string {
  const positional = args.split(/\s+/u).filter(Boolean);
  let expanded = '';
  let consumedArgs = false;
  let rest = template;
  for (;;) {
    const index = rest.indexOf('$');
    if (index < 0) break;
    expanded += rest.slice(0, index);
    const after = rest.slice(index + 1);
    if (after.startsWith('ARGUMENTS')) {
      expanded += args;
      consumedArgs = true;
      rest = after.slice('ARGUMENTS'.length);
    } else if (after.startsWith('@')) {
      expanded += args;
      consumedArgs = true;
      rest = after.slice(1);
    } else if (/^[1-9]/u.test(after)) {
      expanded += positional[Number(after[0]) - 1] ?? '';
      consumedArgs = true;
      rest = after.slice(1);
    } else {
      expanded += '$';
      rest = after;
    }
  }
  expanded += rest;
  return !consumedArgs && args ? `${expanded}\n\n${args}` : expanded;
}

/**
 * The prompt to hand the provider, or null when the text is not a command
 * Waku has to translate. Built-in commands are passed through — the provider
 * understands its own — but template commands expand to their body and
 * skills become the provider's native invocation, since neither survives the
 * trip as a slash command.
 */
export function resolvedComposerSubmission(
  provider: ProviderKind,
  prompt: string,
  commands: SlashCommand[],
): string | null {
  if (!prompt.startsWith('/')) return null;
  const invocation = prompt.slice(1);
  const whitespace = invocation.search(/\s/u);
  const name = whitespace < 0 ? invocation : invocation.slice(0, whitespace);
  const args = whitespace < 0 ? '' : invocation.slice(whitespace).trim();
  const skill = commands.find((command) => command.name === name && command.scope === 'Skill');
  if (skill) {
    if (provider === 'codex' || provider === 'fx') return `$${invocation}`;
    if (provider === 'pi' || provider === 'ohMyPi') return `/skill:${invocation}`;
  }
  const command = commands.find((item) => item.name === name && item.template !== null);
  if (!command?.template) return null;
  return expandCommandTemplate(command.template, args);
}

function commandScopeRank(scope: SlashCommand['scope']): number {
  switch (scope) {
    case 'Builtin':
    case 'Waku':
      return 0;
    case 'Project':
      return 1;
    case 'User':
      return 2;
    case 'Skill':
      return 3;
  }
}
