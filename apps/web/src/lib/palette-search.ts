export { fuzzyScore } from '@waku/client/fuzzy-search'

export function shouldKeepPreviousPaletteItems(
  nextCount: number,
  searchPending: boolean,
  previousCount: number,
): boolean {
  return nextCount === 0 && searchPending && previousCount > 0
}
