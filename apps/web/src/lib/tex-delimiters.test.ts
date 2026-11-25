import { describe, expect, test } from 'bun:test'
import { rewriteTexDelimiters } from '@/lib/tex-delimiters'

describe('rewriteTexDelimiters', () => {
  test('leaves the dollar form alone', () => {
    expect(rewriteTexDelimiters('before $x^2$ after')).toBe('before $x^2$ after')
    expect(rewriteTexDelimiters('before $$x^2$$ after')).toBe('before $$x^2$$ after')
  })

  test('rewrites the LaTeX-style inline form to dollars', () => {
    expect(rewriteTexDelimiters('before \\(x^2\\) after')).toBe('before $x^2$ after')
  })

  test('keeps the formula verbatim', () => {
    // CommonMark eats the backslash of `\{`; the formula must not.
    expect(rewriteTexDelimiters('\\(\\{x \\pm y\\}\\)')).toBe('$\\{x \\pm y\\}$')
  })

  test('reads a display formula on its own line as a block', () => {
    expect(rewriteTexDelimiters('before\n\n\\[x^2\\]\n\nafter')).toBe(
      'before\n\n$$\nx^2\n$$\n\nafter',
    )
    expect(rewriteTexDelimiters('before\n\n$$x^2$$\n\nafter')).toBe(
      'before\n\n$$\nx^2\n$$\n\nafter',
    )
  })

  test('reads a display formula inside a paragraph as inline math', () => {
    expect(rewriteTexDelimiters('before \\[x^2\\] after')).toBe('before $$x^2$$ after')
  })

  test('leaves a display formula that already spans lines alone', () => {
    const source = 'before\n\n$$\n\\frac{a}{b}\n$$\n\nafter'
    expect(rewriteTexDelimiters(source)).toBe(source)
  })

  test('keeps the delimiters literal in code', () => {
    expect(rewriteTexDelimiters('`\\[x\\]`')).toBe('`\\[x\\]`')
    expect(rewriteTexDelimiters('`\\(x\\)`')).toBe('`\\(x\\)`')
    expect(rewriteTexDelimiters('```latex\n\\[x\\]\n```')).toBe('```latex\n\\[x\\]\n```')
    expect(rewriteTexDelimiters('```\n$$x$$\n```')).toBe('```\n$$x$$\n```')
  })

  test('leaves an unclosed delimiter alone', () => {
    // The formula is still streaming: rewriting it would change the rendering
    // of the block when the closer arrives.
    expect(rewriteTexDelimiters('unfinished \\[x')).toBe('unfinished \\[x')
    expect(rewriteTexDelimiters('unfinished \\(x')).toBe('unfinished \\(x')
  })

  test('leaves an escaped backslash alone', () => {
    expect(rewriteTexDelimiters('a \\\\(x')).toBe('a \\\\(x')
  })

  test('leaves a body that carries a dollar alone', () => {
    // A dollar would end the math span early.
    expect(rewriteTexDelimiters('\\(a $ b\\)')).toBe('\\(a $ b\\)')
  })

  test('does not span a paragraph break', () => {
    expect(rewriteTexDelimiters('\\(a\n\nb\\)')).toBe('\\(a\n\nb\\)')
  })

  test('keeps a formula that sits next to prose inline', () => {
    expect(rewriteTexDelimiters('text \\[x\\] more')).toBe('text $$x$$ more')
    expect(rewriteTexDelimiters('text $$x$$ more')).toBe('text $$x$$ more')
  })
})
