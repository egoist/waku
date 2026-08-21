export interface PositionedTranscriptItem {
  index: number
  offset: number
  size: number
}

export function firstVisibleTranscriptItem(
  items: readonly PositionedTranscriptItem[],
  scrollTop: number,
) {
  if (!items.length) return null

  const viewportTop = Math.max(0, scrollTop) + 0.5
  const first = items[0]!
  const last = items[items.length - 1]!
  if (viewportTop < first.offset || viewportTop >= last.offset + last.size) return null

  let low = 0
  let high = items.length
  while (low < high) {
    const middle = (low + high) >>> 1
    const item = items[middle]!
    if (item.offset + item.size <= viewportTop) low = middle + 1
    else high = middle
  }
  return items[low]?.index ?? null
}

export function activeNavigationTurn(
  turns: readonly { itemIndex: number }[],
  firstVisibleItem: number,
  atBottom: boolean,
) {
  if (!turns.length) return null
  if (atBottom) return turns.length - 1

  let low = 0
  let high = turns.length
  while (low < high) {
    const middle = (low + high) >>> 1
    if (turns[middle]!.itemIndex <= firstVisibleItem) low = middle + 1
    else high = middle
  }
  return Math.max(0, low - 1)
}
