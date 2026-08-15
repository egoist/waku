import { describe, expect, test } from 'bun:test'
import type { ProviderProbeResult } from './provider-probe-cache'
import {
  readProviderProbeCache,
  writeProviderProbeCache,
} from './provider-probe-cache'

class MemoryStorage {
  private readonly values = new Map<string, string>()

  getItem(key: string) {
    return this.values.get(key) ?? null
  }

  setItem(key: string, value: string) {
    this.values.set(key, value)
  }
}

const probe: ProviderProbeResult = {
  provider: 'codex',
  installed: true,
  path: '/usr/local/bin/codex',
  models: [{
    id: 'gpt-5',
    name: 'GPT-5',
    is_default: true,
    reasoning_efforts: [],
    service_tiers: [],
  }],
  agent_presets: [],
  version: '1.2.3',
}

describe('provider probe cache', () => {
  test('restores a daemon-scoped model catalog and timestamp', () => {
    const storage = new MemoryStorage()
    writeProviderProbeCache(storage, 'ws://daemon-a', 'codex', null, probe, 10_000)

    expect(readProviderProbeCache(storage, 'ws://daemon-a', 'codex', null, 20_000)).toEqual({
      binaryOverride: null,
      data: probe,
      updatedAt: 10_000,
    })
    expect(readProviderProbeCache(storage, 'ws://daemon-b', 'codex', null, 20_000)).toBeUndefined()
  })

  test('does not reuse a catalog after its binary override changes', () => {
    const storage = new MemoryStorage()
    writeProviderProbeCache(storage, 'ws://daemon-a', 'codex', '/opt/codex', probe, 10_000)

    expect(readProviderProbeCache(storage, 'ws://daemon-a', 'codex', '/other/codex', 20_000)).toBeUndefined()
    expect(readProviderProbeCache(storage, 'ws://daemon-a', 'codex', undefined, 20_000)?.data).toEqual(probe)
  })

  test('ignores expired and malformed cache entries', () => {
    const storage = new MemoryStorage()
    writeProviderProbeCache(storage, 'ws://daemon-a', 'codex', null, probe, 10_000)
    expect(readProviderProbeCache(storage, 'ws://daemon-a', 'codex', null, 31 * 24 * 60 * 60 * 1_000)).toBeUndefined()

    storage.setItem('waku.provider-probes.v1', '{bad json')
    expect(readProviderProbeCache(storage, 'ws://daemon-a', 'codex')).toBeUndefined()
  })
})
