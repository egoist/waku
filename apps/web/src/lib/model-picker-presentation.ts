import type { ProviderKind, ProviderModel } from '@waku/client'

export interface ModelPickerRow {
  provider: ProviderKind
  model: ProviderModel
}

export function modelPickerProviderState({
  current,
  restricted,
  installed,
  isPending,
}: {
  current: boolean
  restricted: boolean
  installed: boolean | undefined
  isPending: boolean
}) {
  const pending = (current || !restricted) && installed === undefined && isPending
  return {
    enabled: current || (!restricted && (installed === true || pending)),
    pending,
  }
}

export function selectedModelPickerIndex(
  rows: readonly ModelPickerRow[],
  provider: ProviderKind,
  modelId: string | undefined,
) {
  if (!modelId) return -1
  return rows.findIndex((row) => row.provider === provider && row.model.id === modelId)
}

export function nextModelPickerHighlight(
  current: number | null,
  length: number,
  direction: 'next' | 'previous',
) {
  if (!length) return null
  if (current === null) return direction === 'next' ? 0 : length - 1
  return direction === 'next'
    ? (current + 1) % length
    : (current - 1 + length) % length
}
