import type { ProviderKind, ProviderProbe } from '@waku/client'

const CACHE_KEY = 'waku.provider-probes.v1'
const CACHE_VERSION = 1
const CACHE_MAX_AGE = 30 * 24 * 60 * 60 * 1_000
const CACHE_MAX_ENTRIES = 32

export const PROVIDER_PROBE_CACHE_STALE_TIME = 24 * 60 * 60 * 1_000

export type ProviderProbeResult = ProviderProbe & { version: string | null }

export interface CachedProviderProbe {
  binaryOverride: string | null
  data: ProviderProbeResult
  updatedAt: number
}

interface ProviderProbeCacheState {
  version: typeof CACHE_VERSION
  entries: Record<string, CachedProviderProbe>
}

type StorageLike = Pick<Storage, 'getItem' | 'setItem'>

export function readProviderProbeCache(
  storage: StorageLike | null,
  address: string,
  provider: ProviderKind,
  expectedBinaryOverride?: string | null,
  now = Date.now(),
): CachedProviderProbe | undefined {
  const state = readState(storage)
  const entry = state?.entries[entryKey(address, provider)]
  if (!entry || now - entry.updatedAt > CACHE_MAX_AGE) return undefined
  if (expectedBinaryOverride !== undefined && entry.binaryOverride !== expectedBinaryOverride) {
    return undefined
  }
  return entry
}

export function writeProviderProbeCache(
  storage: StorageLike | null,
  address: string,
  provider: ProviderKind,
  binaryOverride: string | null,
  data: ProviderProbeResult,
  now = Date.now(),
): void {
  if (!storage) return
  const state = readState(storage) ?? { version: CACHE_VERSION, entries: {} }
  const entries = Object.fromEntries(
    Object.entries(state.entries)
      .filter(([, entry]) => now - entry.updatedAt <= CACHE_MAX_AGE)
      .sort(([, left], [, right]) => right.updatedAt - left.updatedAt)
      .slice(0, CACHE_MAX_ENTRIES - 1),
  )
  entries[entryKey(address, provider)] = { binaryOverride, data, updatedAt: now }
  try {
    storage.setItem(CACHE_KEY, JSON.stringify({ version: CACHE_VERSION, entries }))
  } catch {
    // A catalog cache is disposable; quota and privacy-mode failures are safe.
  }
}

export function browserProviderProbeStorage(): StorageLike | null {
  if (typeof window === 'undefined') return null
  try {
    return window.localStorage
  } catch {
    return null
  }
}

function readState(storage: StorageLike | null): ProviderProbeCacheState | undefined {
  if (!storage) return undefined
  try {
    const parsed = JSON.parse(storage.getItem(CACHE_KEY) ?? 'null') as unknown
    if (!isRecord(parsed) || parsed.version !== CACHE_VERSION || !isRecord(parsed.entries)) {
      return undefined
    }
    const entries: Record<string, CachedProviderProbe> = {}
    for (const [key, value] of Object.entries(parsed.entries)) {
      if (isCachedProviderProbe(value)) entries[key] = value
    }
    return { version: CACHE_VERSION, entries }
  } catch {
    return undefined
  }
}

function isCachedProviderProbe(value: unknown): value is CachedProviderProbe {
  if (!isRecord(value) || typeof value.updatedAt !== 'number') return false
  if (value.binaryOverride !== null && typeof value.binaryOverride !== 'string') return false
  const data = value.data
  return isRecord(data)
    && typeof data.provider === 'string'
    && typeof data.installed === 'boolean'
    && (data.path === null || typeof data.path === 'string')
    && Array.isArray(data.models)
    && Array.isArray(data.agent_presets)
    && (data.version === null || typeof data.version === 'string')
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function entryKey(address: string, provider: ProviderKind): string {
  return `${address}\u0000${provider}`
}
