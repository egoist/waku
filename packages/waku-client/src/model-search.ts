import type { ProviderModel } from './generated'

/**
 * Match a model against a search query the way the desktop does: every
 * whitespace-separated token has to appear somewhere in the model's
 * searchable text, and the tokens need neither be adjacent nor in order.
 *
 * A contiguous match is too strict: "claude sonnet" would find nothing,
 * although both words describe one model.
 */
export function modelMatchesSearch(
  model: Pick<ProviderModel, 'name' | 'id' | 'sub_provider'>,
  providerName: string,
  query: string,
): boolean {  const tokens = query.trim().toLowerCase().split(/\s+/u).filter(Boolean)
  if (tokens.length === 0) return true
  const searchable = [
    model.name,
    model.id,
    model.sub_provider ?? '',
    providerName,
  ].join(' ').toLowerCase()
  return tokens.every((token) => searchable.includes(token))
}
