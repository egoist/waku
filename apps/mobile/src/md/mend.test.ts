import { describe, expect, test } from 'bun:test';

import { closeHanging, PENDING_LINK_URL } from './mend';

const ZERO_WIDTH_SPACE = '​';

describe('closeHanging', () => {
  test('settled text needs no repair', () => {
    for (const text of [
      'plain text',
      '**bold** and *em* and `code`',
      '~~struck~~ through',
      '[link](https://example.com) done',
      'a * b * c',
      '',
    ]) {
      expect(closeHanging(text)).toBeNull();
    }
  });

  test('closes hanging emphasis', () => {
    expect(closeHanging('now **bold')).toBe('now **bold**');
    expect(closeHanging('an *em')).toBe('an *em*');
    expect(closeHanging('an _em')).toBe('an _em_');
    expect(closeHanging('__strong')).toBe('__strong__');
    expect(closeHanging('~~struck')).toBe('~~struck~~');
  });

  test('closes nested emphasis innermost first', () => {
    expect(closeHanging('**bold *both')).toBe('**bold *both***');
  });

  test('completes a half-streamed closer', () => {
    expect(closeHanging('**bold*')).toBe('**bold**');
  });

  test('openers without content stay literal', () => {
    // Nothing to style yet, so the markers must remain as typed.
    expect(closeHanging('text **')).toBeNull();
    expect(closeHanging('text ** ')).toBeNull();
  });

  test('closes hanging inline code', () => {
    expect(closeHanging('call `foo')).toBe('call `foo`');
    expect(closeHanging('call ``a`b')).toBe('call ``a`b``');
    // A bare opener has no content to style yet.
    expect(closeHanging('call `')).toBeNull();
  });

  test('emphasis inside inline code is literal', () => {
    expect(closeHanging('`a ** b`')).toBeNull();
  });

  test('mends links whose url is still streaming', () => {
    expect(closeHanging('see [docs](https://exa')).toBe(
      `see [docs](${PENDING_LINK_URL})`,
    );
    expect(closeHanging('see [docs')).toBe(`see [docs](${PENDING_LINK_URL})`);
    // A bare `[` has no link text yet.
    expect(closeHanging('see [')).toBeNull();
  });

  test('emphasis inside a streaming link text closes first', () => {
    expect(closeHanging('see [**docs')).toBe(
      `see [**docs**](${PENDING_LINK_URL})`,
    );
  });

  test('escapes keep markers literal', () => {
    expect(closeHanging('literal \\*star')).toBeNull();
  });

  test('defuses a streaming setext underline', () => {
    expect(closeHanging('paragraph\n-')).toBe(`paragraph\n-${ZERO_WIDTH_SPACE}`);
    // A blank line between means it is a list bullet, not an underline.
    expect(closeHanging('paragraph\n\n-')).toBeNull();
    // A settled line break is already unambiguous.
    expect(closeHanging('paragraph\n-\n')).toBeNull();
  });

  /** Streaming any prefix must never throw, and repairs must be idempotent —
   * the scanner runs on every delta, so robustness matters more than
   * precision. */
  test('every prefix of a corpus mends without throwing', () => {
    const corpus =
      'Mixed **bold `code`** and *em*, a [link](https://example.com), ' +
      '~~struck~~, emoji 🎉, accents héllo, and a list:\n\n- one\n- two\n';
    for (let end = 0; end <= corpus.length; end += 1) {
      // Skip positions splitting a surrogate pair, mirroring char boundaries.
      const trailing = corpus.charCodeAt(end);
      if (trailing >= 0xdc00 && trailing <= 0xdfff) continue;
      const prefix = corpus.slice(0, end);
      const mended = closeHanging(prefix);
      if (mended !== null) {
        // Repair only ever inserts markers, except for a streaming link
        // whose half-typed URL is swapped for the sentinel.
        expect(
          mended.length >= prefix.length ||
            mended.endsWith(`(${PENDING_LINK_URL})`),
        ).toBe(true);
        // Idempotence is the convergence guarantee: one pass must leave
        // nothing hanging, or a streamed response would grow markers on
        // every delta.
        expect(closeHanging(mended)).toBeNull();
      }
    }
  });
});
