export function fuzzyScore(query: string, candidate: string): number | null {
  const needle = query.trim().toLocaleLowerCase()
  const haystack = candidate.toLocaleLowerCase()
  if (!needle) return 0

  const contiguous = haystack.indexOf(needle)
  if (contiguous >= 0) {
    // Contiguous matches dominate subsequences; earlier and tighter matches
    // win before recency is considered.
    return 100_000 + needle.length * 1_000 - contiguous
  }

  let score = 0
  let nextNeedle = 0
  let previousMatch = -2
  for (let index = 0; index < haystack.length && nextNeedle < needle.length; index += 1) {
    if (haystack[index] !== needle[nextNeedle]) continue
    score += index === previousMatch + 1 ? 200 : 40
    if (index === 0 || /[\s/_.#-]/.test(haystack[index - 1] ?? '')) score += 100
    score += Math.max(0, 50 - index)
    previousMatch = index
    nextNeedle += 1
  }
  return nextNeedle === needle.length ? score : null
}

export function shouldKeepPreviousPaletteItems(
  nextCount: number,
  searchPending: boolean,
  previousCount: number,
): boolean {
  return nextCount === 0 && searchPending && previousCount > 0
}
