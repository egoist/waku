import { useSyncExternalStore } from 'react'
import englishSource from '../../../../locales/app.yml?raw'
import japaneseSource from '../../../../locales/ja.yml?raw'
import simplifiedChineseSource from '../../../../locales/zh-CN.yml?raw'
import {
  APP_LANGUAGES,
  interpolateTranslation,
  normalizeLanguage,
  parseRustI18nCatalog,
  resolveLanguage,
  type AppLanguage,
  type AppLocale,
  type TranslationParams,
} from './i18n-core'

export { APP_LANGUAGES }
export type { AppLanguage, AppLocale }

const LANGUAGE_STORAGE_KEY = 'waku.language'
const catalogs: Record<AppLocale, Record<string, string>> = {
  en: parseRustI18nCatalog(englishSource, 'en'),
  'zh-CN': parseRustI18nCatalog(simplifiedChineseSource, 'zh-CN'),
  ja: parseRustI18nCatalog(japaneseSource, 'ja'),
}

interface LanguageSnapshot {
  language: AppLanguage
  locale: AppLocale
}

const serverSnapshot: LanguageSnapshot = { language: 'system', locale: 'en' }
let browserSnapshot = readBrowserSnapshot()
const listeners = new Set<() => void>()

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (event) => {
    if (event.key !== LANGUAGE_STORAGE_KEY) return
    updateBrowserSnapshot(normalizeLanguage(event.newValue))
  })
}

export function useI18n() {
  const snapshot = useSyncExternalStore(subscribe, getBrowserSnapshot, getServerSnapshot)
  return {
    ...snapshot,
    setLanguage: setAppLanguage,
    t: (key: string, params?: TranslationParams) => translate(snapshot.locale, key, params),
  }
}

export function translate(locale: AppLocale, key: string, params?: TranslationParams) {
  const value = catalogs[locale][key] ?? catalogs.en[key] ?? key
  return interpolateTranslation(value, params)
}

export function languageLabel(language: AppLanguage, locale: AppLocale) {
  switch (language) {
    case 'system': return translate(locale, 'language.system')
    case 'en': return 'English'
    case 'zh-CN': return '简体中文'
    case 'ja': return '日本語'
  }
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

function getBrowserSnapshot() {
  return browserSnapshot
}

function getServerSnapshot() {
  return serverSnapshot
}

function setAppLanguage(language: AppLanguage) {
  if (typeof window !== 'undefined') {
    window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language)
  }
  updateBrowserSnapshot(language)
}

function updateBrowserSnapshot(language: AppLanguage) {
  const locale = resolveLanguage(language, browserLanguages())
  if (browserSnapshot.language === language && browserSnapshot.locale === locale) return
  browserSnapshot = { language, locale }
  if (typeof document !== 'undefined') document.documentElement.lang = locale
  for (const listener of listeners) listener()
}

function readBrowserSnapshot(): LanguageSnapshot {
  if (typeof window === 'undefined') return serverSnapshot
  const language = normalizeLanguage(window.localStorage.getItem(LANGUAGE_STORAGE_KEY))
  return { language, locale: resolveLanguage(language, browserLanguages()) }
}

function browserLanguages() {
  if (typeof navigator === 'undefined') return ['en']
  return navigator.languages.length ? navigator.languages : [navigator.language]
}
