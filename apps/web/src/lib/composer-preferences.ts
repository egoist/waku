import type { AgentSession, ProviderKind } from '@waku/client'

const STORAGE_KEY = 'waku.composer-preferences.v1'

type StorageLike = Pick<Storage, 'getItem' | 'setItem'>

export interface RememberedModelTraits {
  reasoningEffort: string | null
  serviceTier: string | null
}

export interface ComposerPreferences {
  lastProvider: ProviderKind
  lastModel: string | null
  lastReasoningEffort: string | null
  lastServiceTier: string | null
  modelTraits: Record<string, RememberedModelTraits>
}

const DEFAULT_PREFERENCES: ComposerPreferences = {
  lastProvider: 'codex',
  lastModel: null,
  lastReasoningEffort: null,
  lastServiceTier: null,
  modelTraits: {},
}

const PROVIDERS = new Set<ProviderKind>([
  'amp',
  'claude',
  'codex',
  'cursor',
  'deepSeek',
  'openCode',
  'grok',
  'pi',
])

export function browserComposerPreferenceStorage(): StorageLike | null {
  if (typeof window === 'undefined') return null
  try {
    return window.localStorage
  } catch {
    return null
  }
}

export function readComposerPreferences(
  storage: StorageLike | null,
  daemonAddress: string,
): ComposerPreferences {
  if (!storage) return { ...DEFAULT_PREFERENCES }
  try {
    const entries = JSON.parse(storage.getItem(STORAGE_KEY) ?? '{}') as Record<string, unknown>
    return parsePreferences(entries[daemonAddress])
  } catch {
    return { ...DEFAULT_PREFERENCES }
  }
}

export function writeComposerPreferences(
  storage: StorageLike | null,
  daemonAddress: string,
  preferences: ComposerPreferences,
): void {
  if (!storage) return
  let entries: Record<string, unknown> = {}
  try {
    const parsed = JSON.parse(storage.getItem(STORAGE_KEY) ?? '{}')
    if (isRecord(parsed)) entries = parsed
  } catch {
    // Replace malformed disposable app state with the current preference.
  }
  entries[daemonAddress] = preferences
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(entries))
  } catch {
    // Composer choices still work when browser storage is unavailable.
  }
}

export function rememberComposerSession(
  preferences: ComposerPreferences,
  session: Pick<AgentSession, 'provider' | 'model' | 'reasoning_effort' | 'service_tier'>,
): ComposerPreferences {
  if (!session.model) return preferences
  const reasoningEffort = session.reasoning_effort ?? null
  const serviceTier = session.service_tier ?? null
  return {
    ...preferences,
    lastProvider: session.provider,
    lastModel: session.model,
    lastReasoningEffort: reasoningEffort,
    lastServiceTier: serviceTier,
    modelTraits: {
      ...preferences.modelTraits,
      [modelKey(session.provider, session.model)]: {
        reasoningEffort,
        serviceTier,
      },
    },
  }
}

export function rememberedModelTraits(
  preferences: ComposerPreferences,
  provider: ProviderKind,
  model: string,
): RememberedModelTraits | undefined {
  return preferences.modelTraits[modelKey(provider, model)]
}

function parsePreferences(value: unknown): ComposerPreferences {
  if (!isRecord(value) || !PROVIDERS.has(value.lastProvider as ProviderKind)) {
    return { ...DEFAULT_PREFERENCES }
  }
  const modelTraits: Record<string, RememberedModelTraits> = {}
  if (isRecord(value.modelTraits)) {
    for (const [key, traits] of Object.entries(value.modelTraits)) {
      if (!isRecord(traits)) continue
      const reasoningEffort = nullableString(traits.reasoningEffort)
      const serviceTier = nullableString(traits.serviceTier)
      if (reasoningEffort !== undefined && serviceTier !== undefined) {
        modelTraits[key] = { reasoningEffort, serviceTier }
      }
    }
  }
  return {
    lastProvider: value.lastProvider as ProviderKind,
    lastModel: nullableString(value.lastModel) ?? null,
    lastReasoningEffort: nullableString(value.lastReasoningEffort) ?? null,
    lastServiceTier: nullableString(value.lastServiceTier) ?? null,
    modelTraits,
  }
}

function nullableString(value: unknown): string | null | undefined {
  return value === null || typeof value === 'string' ? value : undefined
}

function modelKey(provider: ProviderKind, model: string): string {
  return `${provider}\u0000${model}`
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
