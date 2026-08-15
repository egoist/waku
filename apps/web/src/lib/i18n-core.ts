export type AppLanguage = 'system' | 'en' | 'zh-CN' | 'ja'
export type AppLocale = Exclude<AppLanguage, 'system'>
export type TranslationParams = Record<string, string | number>

export const APP_LANGUAGES: readonly AppLanguage[] = ['system', 'en', 'zh-CN', 'ja']

export function parseRustI18nCatalog(source: string, locale: AppLocale) {
  const messages: Record<string, string> = {}
  let parentKey: string | null = null

  for (const line of source.split(/\r?\n/)) {
    if (!line.trim() || line.trimStart().startsWith('#')) continue

    const nested = line.match(/^\s+([A-Za-z][\w-]*):(?:\s(.*))?$/)
    if (nested && parentKey) {
      if (nested[1] === locale && nested[2] !== undefined) {
        messages[parentKey] = parseYamlScalar(nested[2])
      }
      continue
    }

    const root = line.match(/^([^\s][^:]*):(?:\s(.*))?$/)
    if (!root) continue
    const key = root[1]!
    const value = root[2]
    parentKey = value === undefined ? key : null
    if (key === '_version' || value === undefined) continue
    messages[key] = parseYamlScalar(value)
  }

  return messages
}

export function normalizeLanguage(value: string | null | undefined): AppLanguage {
  return APP_LANGUAGES.includes(value as AppLanguage) ? value as AppLanguage : 'system'
}

export function resolveLanguage(
  language: AppLanguage,
  preferredLanguages: readonly string[],
): AppLocale {
  if (language !== 'system') return language
  for (const preferred of preferredLanguages) {
    const locale = preferred.replaceAll('_', '-').toLowerCase()
    if (locale === 'zh-cn' || locale === 'zh-sg' || locale.startsWith('zh-hans')) {
      return 'zh-CN'
    }
    if (locale === 'ja' || locale.startsWith('ja-')) return 'ja'
  }
  return 'en'
}

export function interpolateTranslation(value: string, params: TranslationParams = {}) {
  return value.replace(/%\{([A-Za-z0-9_]+)\}/g, (placeholder, name: string) => (
    Object.hasOwn(params, name) ? String(params[name]) : placeholder
  ))
}

function parseYamlScalar(value: string) {
  const trimmed = value.trim()
  if (!trimmed.startsWith('"')) return trimmed
  try {
    return JSON.parse(trimmed) as string
  } catch {
    throw new Error(`Invalid quoted locale value: ${trimmed}`)
  }
}
