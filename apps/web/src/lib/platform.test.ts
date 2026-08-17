import { describe, expect, test } from 'bun:test'
import { isMacLikePlatform } from './platform'

describe('web shortcut platform', () => {
  test('uses Command labels on Apple platforms', () => {
    expect(isMacLikePlatform('macOS')).toBe(true)
    expect(isMacLikePlatform('MacIntel')).toBe(true)
    expect(isMacLikePlatform('iPad')).toBe(true)
  })

  test('uses Control labels elsewhere', () => {
    expect(isMacLikePlatform('Windows')).toBe(false)
    expect(isMacLikePlatform('Linux x86_64')).toBe(false)
    expect(isMacLikePlatform('')).toBe(false)
  })
})
