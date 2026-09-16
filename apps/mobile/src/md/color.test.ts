import { describe, expect, test } from 'bun:test';

import { applyAlpha } from './color';

describe('applyAlpha', () => {
  test('covers every theme color format', () => {
    expect(applyAlpha('#242424', 0.5)).toBe('rgba(36, 36, 36, 0.5)');
    expect(applyAlpha('#e2e2e2', 0)).toBe('rgba(226, 226, 226, 0)');
    expect(applyAlpha('#abc', 0.5)).toBe('rgba(170, 187, 204, 0.5)');
    expect(applyAlpha('#24242480', 0.5)).toBe('rgba(36, 36, 36, 0.251)');
    expect(applyAlpha('rgba(28, 31, 37, 0.10)', 0.5)).toBe('rgba(28, 31, 37, 0.05)');
    expect(applyAlpha('rgb(28, 31, 37)', 0.5)).toBe('rgba(28, 31, 37, 0.5)');
    expect(applyAlpha('hsla(220, 10%, 12%, 0.08)', 0.5)).toBe('hsla(220, 10%, 12%, 0.04)');
    expect(applyAlpha('hsl(220, 10%, 12%)', 0.25)).toBe('hsla(220, 10%, 12%, 0.25)');
  });

  test('full opacity and unknown formats pass through', () => {
    expect(applyAlpha('#242424', 1)).toBe('#242424');
    expect(applyAlpha('papayawhip', 0.5)).toBe('papayawhip');
  });
});
