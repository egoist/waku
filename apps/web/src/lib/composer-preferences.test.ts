import { describe, expect, test } from 'bun:test'
import {
  newTaskProvider,
  readComposerPreferences,
  rememberedModelTraits,
  rememberComposerSession,
  writeComposerPreferences,
} from './composer-preferences'

describe('composer preferences', () => {
  test('remembers the last model per daemon and its traits', () => {
    const storage = memoryStorage()
    const remembered = rememberComposerSession(
      readComposerPreferences(storage, 'ws://first'),
      {
        provider: 'codex',
        model: 'gpt-5.6-sol',
        reasoning_effort: 'high',
        service_tier: 'fast',
        context_window: '1m',
      },
    )
    writeComposerPreferences(storage, 'ws://first', remembered)

    expect(readComposerPreferences(storage, 'ws://first')).toMatchObject({
      lastProvider: 'codex',
      lastModel: 'gpt-5.6-sol',
      lastReasoningEffort: 'high',
      lastServiceTier: 'fast',
      lastContextWindow: '1m',
    })
    expect(rememberedModelTraits(remembered, 'codex', 'gpt-5.6-sol')).toEqual({
      reasoningEffort: 'high',
      serviceTier: 'fast',
      contextWindow: '1m',
    })
    expect(readComposerPreferences(storage, 'ws://second').lastModel).toBeNull()
  })

  test('does not erase an explicit model when a blank draft is selected', () => {
    const preferences = rememberComposerSession(
      readComposerPreferences(null, 'ws://first'),
      {
        provider: 'claude',
        model: 'claude-opus-4-1',
        reasoning_effort: null,
        service_tier: null,
      },
    )
    expect(rememberComposerSession(preferences, {
      provider: 'codex',
      model: null,
      reasoning_effort: null,
      service_tier: null,
    })).toBe(preferences)
  })

  test('restores Fx as a valid remembered provider', () => {
    const storage = memoryStorage()
    const preferences = rememberComposerSession(
      readComposerPreferences(storage, 'ws://first'),
      {
        provider: 'fx',
        model: 'openai/gpt-5.6-sol',
        reasoning_effort: null,
        service_tier: null,
      },
    )
    writeComposerPreferences(storage, 'ws://first', preferences)

    expect(readComposerPreferences(storage, 'ws://first').lastProvider).toBe('fx')
  })

  test('carries preferences saved for the OpenCode 2 beta over to OpenCode', () => {
    const storage = memoryStorage()
    storage.setItem('waku.composer-preferences.v1', JSON.stringify({
      'ws://first': {
        lastProvider: 'openCode2',
        lastModel: 'opencode-go/deepseek-v4-flash',
        lastReasoningEffort: 'high',
        lastServiceTier: null,
        modelTraits: {
          'openCode2\u0000opencode-go/deepseek-v4-flash': {
            reasoningEffort: 'high',
            serviceTier: null,
          },
        },
      },
    }))

    const preferences = readComposerPreferences(storage, 'ws://first')
    expect(preferences.lastProvider).toBe('openCode')
    expect(preferences.lastModel).toBe('opencode-go/deepseek-v4-flash')
    expect(rememberedModelTraits(preferences, 'openCode', 'opencode-go/deepseek-v4-flash')).toEqual({
      reasoningEffort: 'high',
      serviceTier: null,
      contextWindow: null,
    })
  })
})

describe('newTaskProvider', () => {
  const usable = ['amp', 'claude', 'codex', 'kimi'] as const

  test('keeps a usable remembered provider', () => {
    expect(newTaskProvider('kimi', [...usable])).toBe('kimi')
  })

  test('falls back to the shared default when the remembered provider is off', () => {
    expect(newTaskProvider('fx', [...usable])).toBe('codex')
    expect(newTaskProvider('kimi', ['amp', 'codex'])).toBe('codex')
  })

  test('falls back to codex when codex is the only other usable provider', () => {
    expect(newTaskProvider('claude', ['amp', 'codex'])).toBe('codex')
  })

  test('falls back to the first usable provider when codex is off too', () => {
    expect(newTaskProvider('claude', ['amp', 'kimi'])).toBe('amp')
  })

  test('keeps the remembered provider when nothing is usable', () => {
    expect(newTaskProvider('claude', [])).toBe('claude')
  })
})

function memoryStorage() {
  const values = new Map<string, string>()
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value) },
  }
}
