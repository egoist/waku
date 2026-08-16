import { describe, expect, test } from 'bun:test'
import { normalizeDaemonAddress, validateConnectionConfig } from './connection'

describe('normalizeDaemonAddress', () => {
  test('normalizes host, HTTP, and daemon paths', () => {
    expect(normalizeDaemonAddress('host.example:34123')).toBe(
      'ws://host.example:34123',
    )
    expect(normalizeDaemonAddress('https://waku.example/v1?token=nope')).toBe(
      'wss://waku.example',
    )
    expect(normalizeDaemonAddress('HTTP://WAKU.EXAMPLE/v1')).toBe(
      'ws://waku.example',
    )
  })

  test('rejects unsupported schemes and credentials', () => {
    expect(() => normalizeDaemonAddress('ftp://waku.example')).toThrow()
    expect(() => normalizeDaemonAddress('ws://token@waku.example')).toThrow()
  })

  test('requires a token without putting it in the address', () => {
    expect(() =>
      validateConnectionConfig({ address: 'waku.example', token: '  ' }),
    ).toThrow('token')
    expect(
      validateConnectionConfig({ address: 'waku.example', token: 'secret' }),
    ).toEqual({
      address: 'ws://waku.example',
      token: 'secret',
      remember: false,
    })
  })
})
