import type { FileEntry, MessageAttachment, ProviderKind, SlashCommand } from '@waku/client';
import {
  composerAutocompleteRows,
  expandedComposerSubmission,
  type ComposerAutocompleteRow,
  type ComposerTrigger,
} from '@waku/client/composer-autocomplete';

export interface ComposerSelection {
  start: number;
  end: number;
}

export interface ComposerPickerResults {
  scope: string;
  query: string;
  commands: SlashCommand[];
  files: FileEntry[];
  rows: ComposerAutocompleteRow[];
}

/** Retain the last completed list during a new search in the same workspace.
 * A different daemon, provider, workspace, or picker must never reuse it. */
export function composerPickerSearchState(
  results: ComposerPickerResults | null,
  scope: string,
  query: string,
  commands: SlashCommand[],
  files: FileEntry[],
): { rows: ComposerAutocompleteRow[]; pending: boolean; hasResults: boolean } {
  const sameScope = results?.scope === scope;
  return {
    rows: sameScope ? results.rows : [],
    pending: !sameScope || results.query !== query || results.commands !== commands || results.files !== files,
    hasResults: sameScope,
  };
}

/** Commands own the beginning of a prompt; existing draft text becomes their
 * arguments. File mentions replace the selection or partially typed mention. */
export function insertComposerItem(
  text: string,
  selection: ComposerSelection,
  row: ComposerAutocompleteRow,
): { text: string; cursor: number } {
  if (row.kind === 'command') {
    const insert = `/${row.command.name} `;
    const args = text.replace(/^\/\S*\s?/u, '');
    return { text: insert + args, cursor: insert.length };
  }
  let start = Math.max(0, Math.min(selection.start, text.length));
  const end = Math.max(start, Math.min(selection.end, text.length));
  if (start === end) {
    const mention = text.slice(0, start).match(/(?:^|\s)(@\S*)$/u);
    if (mention) start -= mention[1]!.length;
  }
  const before = text.slice(0, start);
  const insert = `${before && !/\s$/u.test(before) ? ' ' : ''}@${row.file.path} `;
  const after = text.slice(end).replace(/^ /u, '');
  return { text: before + insert + after, cursor: before.length + insert.length };
}

/** Native selection notifications may arrive before or after text changes.
 * Infer the new caret from the edit until the next selection event arrives.
 * All offsets stay in UTF-16, like TextInput and the shared JS matcher. */
export function selectionAfterComposerEdit(
  before: string,
  after: string,
  selection: ComposerSelection,
): ComposerSelection {
  const inserted = after.length - before.length + selection.end - selection.start;
  if (inserted >= 0
    && before.slice(0, selection.start) === after.slice(0, selection.start)
    && before.slice(selection.end) === after.slice(selection.start + inserted)) {
    const cursor = selection.start + inserted;
    return { start: cursor, end: cursor };
  }
  let prefix = 0;
  while (prefix < before.length && prefix < after.length && before[prefix] === after[prefix]) {
    prefix += 1;
  }
  let suffix = 0;
  while (suffix < before.length - prefix && suffix < after.length - prefix
    && before[before.length - suffix - 1] === after[after.length - suffix - 1]) {
    suffix += 1;
  }
  const cursor = after.length - suffix;
  return { start: cursor, end: cursor };
}

/** A large remote checkout must not monopolize the mobile JS thread while
 * typing. Keep only the best 64 matches and yield between bounded batches.
 * Cancellation prevents a superseded query/daemon/workspace from publishing. */
export async function searchComposerRows(
  trigger: ComposerTrigger,
  commands: SlashCommand[],
  files: FileEntry[],
  cancelled: () => boolean,
  yieldTask: () => Promise<void> = () => new Promise((resolve) => setTimeout(resolve, 0)),
): Promise<ComposerAutocompleteRow[] | null> {
  if (trigger.kind === 'command' || !trigger.query.trim()) {
    return cancelled() ? null : composerAutocompleteRows(trigger, commands, files);
  }
  let matches: FileEntry[] = [];
  for (let index = 0; index < files.length; index += 512) {
    if (cancelled()) return null;
    matches = composerAutocompleteRows(trigger, [], [
      ...matches,
      ...files.slice(index, index + 512),
    ]).flatMap((row) => row.kind === 'file' ? [row.file] : []);
    if (index + 512 < files.length) await yieldTask();
  }
  return cancelled() ? null : matches.map((file) => ({ kind: 'file', file }));
}

export function composerProviderPrompt(
  provider: ProviderKind,
  prompt: string,
  commands: SlashCommand[],
  attachments: MessageAttachment[] = [],
): string | undefined {
  const expanded = expandedComposerSubmission(provider, prompt.trim(), commands);
  if (expanded === null) return undefined;
  return [expanded, attachments.map((attachment) => `@${attachment.mention}`).join(' ')]
    .filter(Boolean).join(' ');
}
