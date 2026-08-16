import { describe, expect, test } from 'bun:test'
import { readThemeChoice, resolvedTheme } from './appearance'

describe('appearance preferences', () => {
  test('system follows the current operating-system appearance', () => {
    expect(resolvedTheme('system', true)).toBe('dark')
    expect(resolvedTheme('system', false)).toBe('light')
  })

  test('an explicit appearance overrides the operating system', () => {
    expect(resolvedTheme('light', true)).toBe('light')
    expect(resolvedTheme('dark', false)).toBe('dark')
  })

  test('unknown and missing stored values fall back to system', () => {
    expect(readThemeChoice(null)).toBe('system')
    expect(readThemeChoice({ getItem: () => 'sepia' })).toBe('system')
    expect(readThemeChoice({ getItem: () => 'dark' })).toBe('dark')
  })
})
