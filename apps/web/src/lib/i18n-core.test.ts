import { describe, expect, test } from 'bun:test'
import {
  interpolateTranslation,
  normalizeLanguage,
  parseRustI18nCatalog,
  resolveLanguage,
} from './i18n-core'

describe('web i18n', () => {
  test('keeps every translated catalog in sync with English', async () => {
    const localeRoot = new URL('../../../../locales/', import.meta.url)
    const english = parseRustI18nCatalog(
      await Bun.file(new URL('app.yml', localeRoot)).text(),
      'en',
    )

    for (const [locale, file] of [
      ['zh-CN', 'zh-CN.yml'],
      ['ja', 'ja.yml'],
    ] as const) {
      const translated = parseRustI18nCatalog(
        await Bun.file(new URL(file, localeRoot)).text(),
        locale,
      )
      expect(Object.keys(translated).sort()).toEqual(Object.keys(english).sort())
    }
  })

  test('parses the nested English catalog and flat translated catalogs', () => {
    expect(parseRustI18nCatalog(`
_version: 2
plain:
  en: Plain text
quoted:
  en: "Line one\\nLine two: %{count}"
`, 'en')).toEqual({
      plain: 'Plain text',
      quoted: 'Line one\nLine two: %{count}',
    })
    expect(parseRustI18nCatalog(`
_version: 1
plain: 简体中文
quoted: "值：%{count}"
`, 'zh-CN')).toEqual({ plain: '简体中文', quoted: '值：%{count}' })
  })

  test('matches the desktop system-language resolution', () => {
    expect(resolveLanguage('system', ['zh-Hans-SG', 'en-US'])).toBe('zh-CN')
    expect(resolveLanguage('system', ['zh-Hant-TW'])).toBe('en')
    expect(resolveLanguage('system', ['ja-JP'])).toBe('ja')
    expect(resolveLanguage('system', ['fr-FR'])).toBe('en')
    expect(resolveLanguage('zh-CN', ['en-US'])).toBe('zh-CN')
  })

  test('normalizes persisted values and interpolates named parameters', () => {
    expect(normalizeLanguage('ja')).toBe('ja')
    expect(normalizeLanguage('invalid')).toBe('system')
    expect(interpolateTranslation('%{count} files in %{project}', {
      count: 2,
      project: 'waku',
    })).toBe('2 files in waku')
  })
})
