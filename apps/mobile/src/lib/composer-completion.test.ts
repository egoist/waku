import { describe, expect, test } from 'bun:test';
import type { FileEntry, SlashCommand } from '@waku/client';
import {
  composerAutocompleteRows,
  detectComposerTrigger,
  replaceComposerTrigger,
} from '@waku/client/composer-autocomplete';

import { composerPickerSearchState, insertComposerItem, composerProviderPrompt, searchComposerRows, selectionAfterComposerEdit } from './composer-completion';
import { beginTurn, createSession, queueSubmission } from './mobile-runtime';

describe('mobile composer selection', () => {
  const command = { kind: 'command' as const, command: {
    name: 'review', description: '', scope: 'Project' as const, template: 'Review $ARGUMENTS', argument_hint: null,
  } };
  const file = { kind: 'file' as const, file: { path: 'src/app.ts', is_dir: false } };

  test('adds a chosen command before the draft and replaces an existing command', () => {
    expect(insertComposerItem('', { start: 0, end: 0 }, command))
      .toEqual({ text: '/review ', cursor: 8 });
    expect(insertComposerItem('check this change', { start: 17, end: 17 }, command))
      .toEqual({ text: '/review check this change', cursor: 8 });
    expect(insertComposerItem('/old check this change', { start: 21, end: 21 }, command))
      .toEqual({ text: '/review check this change', cursor: 8 });
  });

  test('inserts a picked file at the caret or in place of selected text', () => {
    expect(insertComposerItem('read this next', { start: 5, end: 9 }, file))
      .toEqual({ text: 'read @src/app.ts next', cursor: 17 });
    expect(insertComposerItem('read', { start: 4, end: 4 }, file))
      .toEqual({ text: 'read @src/app.ts ', cursor: 17 });
    expect(insertComposerItem('🔎 next', { start: 3, end: 3 }, file))
      .toEqual({ text: '🔎 @src/app.ts next', cursor: 15 });
  });

  test('replaces a partial mention even when it is an argument to a command', () => {
    expect(insertComposerItem('/review @sr', { start: 11, end: 11 }, file))
      .toEqual({ text: '/review @src/app.ts ', cursor: 20 });
    expect(insertComposerItem('look @sr next', { start: 8, end: 8 }, file))
      .toEqual({ text: 'look @src/app.ts next', cursor: 17 });
  });

  test('tracks insertion, backspace, and replacing a selection in the middle', () => {
    expect(selectionAfterComposerEdit('read @app next', 'read @apps next', { start: 9, end: 9 }))
      .toEqual({ start: 10, end: 10 });
    expect(selectionAfterComposerEdit('read @apps next', 'read @app next', { start: 10, end: 10 }))
      .toEqual({ start: 9, end: 9 });
    expect(selectionAfterComposerEdit('read @apps next', 'read @src next', { start: 6, end: 10 }))
      .toEqual({ start: 9, end: 9 });
  });

  test('handles native selection events arriving before the changed text', () => {
    expect(selectionAfterComposerEdit('read @app next', 'read @apps next', { start: 10, end: 10 }))
      .toEqual({ start: 10, end: 10 });
  });

  test('keeps repeated-text insertions anchored at the actual caret', () => {
    expect(selectionAfterComposerEdit('@aaa', '@aaaa', { start: 2, end: 2 }))
      .toEqual({ start: 3, end: 3 });
  });

  test('completes a mention after emoji without truncating neighboring text', () => {
    const text = '🔎 @源 next';
    const trigger = detectComposerTrigger(text, 5)!;
    expect(trigger).toEqual({ kind: 'file', query: '源', start: 3, end: 5 });
    expect(replaceComposerTrigger(text, trigger, {
      kind: 'file', file: { path: '源/文件.ts', is_dir: false },
    })).toEqual({ text: '🔎 @源/文件.ts  next', cursor: 12 });
    expect(detectComposerTrigger('hello@example.com', 17)).toBeNull();
    expect(detectComposerTrigger('say /review', 11)).toBeNull();
  });
});

describe('mobile composer search', () => {
  const files: FileEntry[] = Array.from({ length: 2_500 }, (_, index) => ({
    path: `src/components/${index}/composer-input.tsx`, is_dir: false,
  }));
  const trigger = { kind: 'file' as const, query: 'cmpin', start: 0, end: 6 };

  test('keeps the last file rows while a new query or file index is being filtered', () => {
    const commands: SlashCommand[] = [];
    const rows = composerAutocompleteRows(trigger, commands, files);
    const completed = { scope: 'daemon:repo:file', query: 'cmpin', commands, files, rows };
    const nextQuery = composerPickerSearchState(completed, completed.scope, 'cmpinput', commands, files);
    expect(nextQuery.pending).toBe(true);
    expect(nextQuery.rows).toBe(rows);
    expect(nextQuery.hasResults).toBe(true);
    const refreshedIndex = composerPickerSearchState(completed, completed.scope, completed.query, commands, [...files]);
    expect(refreshedIndex.pending).toBe(true);
    expect(refreshedIndex.rows).toBe(rows);
  });

  test('publishes a completed empty result and never retains another workspace’s rows', () => {
    const commands: SlashCommand[] = [];
    const completed = { scope: 'daemon:repo:file', query: 'missing', commands, files, rows: [] };
    expect(composerPickerSearchState(completed, completed.scope, 'missing', commands, files))
      .toEqual({ rows: [], pending: false, hasResults: true });
    const old = { ...completed, rows: composerAutocompleteRows(trigger, commands, files) };
    expect(composerPickerSearchState(old, 'daemon:other-repo:file', '', commands, files))
      .toEqual({ rows: [], pending: true, hasResults: false });
  });

  test('keeps desktop ranking and cap while yielding for large indexes', async () => {
    let yields = 0;
    const result = await searchComposerRows(trigger, [], files, () => false, async () => { yields += 1; });
    expect(result).toEqual(composerAutocompleteRows(trigger, [], files));
    expect(result).toHaveLength(64);
    expect(yields).toBeGreaterThan(0);
  });

  test('discards a search superseded by a keystroke or workspace change', async () => {
    let cancelled = false;
    const result = await searchComposerRows(trigger, [], files, () => cancelled, async () => { cancelled = true; });
    expect(result).toBeNull();
  });

  test('includes folders and limits an unfiltered file index without scheduling a scan', async () => {
    const folder = { path: 'src/', is_dir: true };
    expect(await searchComposerRows({ ...trigger, query: '' }, [], [folder, ...files], () => false))
      .toEqual([folder, ...files].slice(0, 64).map((file) => ({ kind: 'file', file })));
  });
});

describe('mobile slash command submission', () => {
  const template: SlashCommand = {
    name: 'review', description: 'Review files', scope: 'Project',
    template: 'Review $ARGUMENTS', argument_hint: '[files]',
  };
  const skill: SlashCommand = { ...template, name: 'deploy', scope: 'Skill', template: null };
  const clock = { nowSeconds: () => 42, randomUUID: () => crypto.randomUUID() };

  test('expands templates while preserving typed slash text in new turns and queued messages', () => {
    const session = createSession('project', 'codex', false, clock);
    const attachment = { path: '/tmp/photo.png', mention: '/tmp/photo.png', name: 'photo.png', is_dir: false, is_image: true };
    const expanded = composerProviderPrompt('codex', '/review @src/', [template], [attachment]);
    expect(expanded).toBe('Review @src/ @/tmp/photo.png');
    expect(beginTurn(session, '/review @src/', clock, [attachment], expanded).messages[0])
      .toMatchObject({ content: expanded, display_content: '/review @src/', attachments: [attachment] });
    expect(queueSubmission(session, '/review @src/', clock, [attachment], expanded).queued_messages?.[0])
      .toMatchObject({ content: expanded, display_content: '/review @src/', attachments: [attachment] });
  });

  test('uses provider-native skill syntax and leaves native command templates to the provider', () => {
    expect(composerProviderPrompt('codex', '/deploy prod', [skill])).toBe('$deploy prod');
    expect(composerProviderPrompt('fx', '/deploy prod', [skill])).toBe('$deploy prod');
    expect(composerProviderPrompt('pi', '/deploy prod', [skill])).toBe('/skill:deploy prod');
    expect(composerProviderPrompt('ohMyPi', '/deploy prod', [skill])).toBe('/skill:deploy prod');
    expect(composerProviderPrompt('claude', '/deploy prod', [skill])).toBeUndefined();
    expect(composerProviderPrompt('openCode2', '/review @src', [{ ...template, template: null }])).toBeUndefined();
    expect(composerProviderPrompt('codex', '/unknown', [template])).toBeUndefined();
  });
});
