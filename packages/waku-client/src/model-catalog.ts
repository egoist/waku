/**
 * Cursor model-id resolution, a port of the desktop's
 * `crates/waku-protocol/src/model_catalog.rs`.
 *
 * Cursor's public model ids are often a base slug plus a hyphenated variant
 * suffix (`thinking`, `xhigh`, `fast`). The parameterized ACP picker advertises
 * the base slug, and the unconsumed suffix is applied as separate config
 * options. A session that stored a suffixed id therefore resolves to a catalog
 * model here while an exact `find` would miss it entirely.
 */
import type { ProviderKind, ProviderModel, ProviderModelOption } from './generated'

export interface CursorModelSelection {
  value: string
  suffix: string
}

export interface CatalogModelMatch {
  model: ProviderModel
  suffix: string
}

/**
 * CLI-facing aliases for a requested Cursor model id.
 *
 * Cursor's CLI spells a few families as `claude-4.6-sonnet-*` while ACP
 * advertises `claude-sonnet-4-6`. `auto` and `default` name the same route.
 */
export function cursorModelAliases(requested: string): string[] {
  const aliases = [requested]
  const stripped = requested.startsWith('cursor-') ? requested.slice('cursor-'.length) : null
  if (stripped !== null) aliases.push(stripped)
  if (requested === 'auto') aliases.push('default')
  else if (requested === 'default') aliases.push('auto')
  if (requested.startsWith('claude-')) {
    const rest = requested.slice('claude-'.length)
    const firstDash = rest.indexOf('-')
    if (firstDash >= 0) {
      const version = rest.slice(0, firstDash)
      const familyAndSuffix = rest.slice(firstDash + 1)
      const secondDash = familyAndSuffix.indexOf('-')
      const family = secondDash < 0 ? familyAndSuffix : familyAndSuffix.slice(0, secondDash)
      const suffix = secondDash < 0 ? '' : familyAndSuffix.slice(secondDash + 1)
      if (family === 'haiku' || family === 'opus' || family === 'sonnet') {
        let alias = `claude-${family}-${version.replaceAll('.', '-')}`
        if (suffix) alias += `-${suffix}`
        if (!aliases.includes(alias)) aliases.push(alias)
      }
    }
  }
  return aliases
}

/**
 * Resolves a requested Cursor model id against advertised base values.
 *
 * Exact matches win. Otherwise the longest advertised value that is a hyphen
 * prefix of an alias is used, and the remainder is the variant suffix.
 */
export function resolveCursorModel(
  values: readonly string[],
  requested: string,
): CursorModelSelection | undefined {
  const aliases = cursorModelAliases(requested)
  for (const alias of aliases) {
    const value = values.find((candidate) => candidate === alias)
    if (value !== undefined) return { value, suffix: '' }
  }
  let best: CursorModelSelection | undefined
  for (const alias of aliases) {
    for (const value of values) {
      const suffix = alias.startsWith(`${value}-`) ? alias.slice(value.length + 1) : null
      // Rust's `max_by_key` keeps the last maximum, so ties go to the later
      // advertised value here too.
      if (suffix !== null && (!best || value.length >= best.value.length)) {
        best = { value, suffix }
      }
    }
  }
  return best
}

/**
 * Resolves a stored model id against a provider's catalog the way the desktop
 * does: exact match for every provider, base-model-plus-suffix matching for
 * Cursor. Returns `undefined` when nothing matches.
 */
export function catalogModel(
  provider: ProviderKind,
  models: readonly ProviderModel[],
  requested: string | null | undefined,
): CatalogModelMatch | undefined {
  if (requested == null) return undefined
  if (provider === 'cursor') {
    const selection = resolveCursorModel(
      models.map((model) => model.id),
      requested,
    )
    const model = selection && models.find((candidate) => candidate.id === selection.value)
    return model && selection ? { model, suffix: selection.suffix } : undefined
  }
  const model = models.find((candidate) => candidate.id === requested)
  return model ? { model, suffix: '' } : undefined
}

function suffixHas(suffix: string, value: string): boolean {
  return suffix.split('-').some((part) => part === value)
}

/**
 * The reasoning effort a Cursor variant suffix carries, if the catalog model
 * offers it. Extra-high is advertised as both `xhigh` and `extra-high` and
 * wins over lesser efforts spelled in the same suffix.
 */
export function cursorSuffixReasoningEffort(
  suffix: string,
  efforts: readonly ProviderModelOption[],
): string | undefined {
  if (!suffix || efforts.length === 0) return undefined
  if (
    (suffix.includes('extra-high') || suffixHas(suffix, 'xhigh'))
    && efforts.some((option) => option.id === 'xhigh')
  ) {
    return 'xhigh'
  }
  return efforts.find(
    (option) => suffixHas(suffix, option.id)
      || (option.id === 'xhigh' && suffix.includes('extra-high')),
  )?.id
}

/** The service tier a Cursor variant suffix carries (`fast`). */
export function cursorSuffixServiceTier(
  suffix: string,
  tiers: readonly ProviderModelOption[],
): string | undefined {
  return suffixHas(suffix, 'fast') && tiers.some((option) => option.id === 'fast')
    ? 'fast'
    : undefined
}
