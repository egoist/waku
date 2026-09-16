import { describe, expect, test } from 'bun:test';

import { diffStats, parseUnifiedDiff } from './diff-presentation';

describe('unified diff parsing', () => {
  test('classifies added, removed, context, and hunk lines', () => {
    const { lines, truncated } = parseUnifiedDiff([
      '--- a/src/a.ts',
      '+++ b/src/a.ts',
      '@@ -1,3 +1,3 @@',
      ' unchanged',
      '-old line',
      '+new line',
    ].join('\n'));
    expect(truncated).toBe(false);
    expect(lines).toEqual([
      {
        kind: 'hunk',
        text: '@@ -1,3 +1,3 @@',
        oldLine: null,
        newLine: null,
      },
      { kind: 'context', text: 'unchanged', oldLine: 1, newLine: 1 },
      { kind: 'remove', text: 'old line', oldLine: 2, newLine: null },
      { kind: 'add', text: 'new line', oldLine: null, newLine: 2 },
    ]);
    expect(diffStats(lines)).toEqual({ additions: 1, deletions: 1 });
  });

  test('accepts bare @@ hunks from string-replacement edit tools', () => {
    const { lines } = parseUnifiedDiff('@@\n-before\n+after');
    expect(lines.map((line) => line.kind)).toEqual(['hunk', 'remove', 'add']);
  });

  test('drops git headers and no-newline markers', () => {
    const { lines } = parseUnifiedDiff([
      'diff --git a/x b/x',
      'index 123..456 100644',
      '--- a/x',
      '+++ b/x',
      '@@ -1 +1 @@',
      '-a',
      '+b',
      '\\ No newline at end of file',
    ].join('\n'));
    expect(lines.map((line) => line.kind)).toEqual(['hunk', 'remove', 'add']);
  });

  test('truncates very large diffs', () => {
    const body = Array.from({ length: 500 }, (_, index) => `+line ${index}`).join('\n');
    const { lines, truncated } = parseUnifiedDiff(`@@\n${body}`);
    expect(truncated).toBe(true);
    expect(lines.length).toBe(400);
  });

  test('numbers old and new sides without using marker rows', () => {
    const { lines } = parseUnifiedDiff([
      '@@ -40,3 +40,4 @@',
      ' before',
      '-old',
      '+new',
      '+another',
      ' after',
    ].join('\n'));

    expect(lines.slice(1).map(({ kind, oldLine, newLine }) => ({
      kind,
      oldLine,
      newLine,
    }))).toEqual([
      { kind: 'context', oldLine: 40, newLine: 40 },
      { kind: 'remove', oldLine: 41, newLine: null },
      { kind: 'add', oldLine: null, newLine: 41 },
      { kind: 'add', oldLine: null, newLine: 42 },
      { kind: 'context', oldLine: 42, newLine: 43 },
    ]);
  });

  test('review presentation keeps three context lines around changes', () => {
    const body = Array.from({ length: 20 }, (_, index) => {
      const line = index + 1;
      if (line === 8) return '-old eight\n+new eight';
      return ` line ${line}`;
    }).join('\n');
    const { lines } = parseUnifiedDiff(
      `@@ -1,20 +1,20 @@\n${body}`,
      { contextLines: 3, hidePositionedHunks: true },
    );

    expect(lines.map((line) =>
      line.kind === 'gap'
        ? `gap:${line.hiddenLines}`
        : `${line.kind}:${line.newLine ?? line.oldLine}`,
    )).toEqual([
      'gap:4',
      'context:5',
      'context:6',
      'context:7',
      'remove:8',
      'add:8',
      'context:9',
      'context:10',
      'context:11',
      'gap:9',
    ]);
  });
});
