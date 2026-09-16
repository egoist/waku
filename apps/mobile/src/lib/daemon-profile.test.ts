import { describe, expect, test } from 'bun:test';

import {
  displayHost,
  isPrivateDaemonAddress,
  normalizeDaemonAddress,
  normalizeDaemonProfile,
  parseDaemonProfiles,
  profileInitials,
} from './daemon-profile';

describe('daemon profiles', () => {
  test('normalizes host, HTTP aliases, and protocol paths', () => {
    expect(normalizeDaemonAddress('waku.local:34123')).toBe('ws://waku.local:34123');
    expect(normalizeDaemonAddress('https://waku.example.com/v1?old=1')).toBe(
      'wss://waku.example.com',
    );
  });

  test('rejects unsupported schemes and embedded credentials', () => {
    expect(() => normalizeDaemonAddress('ftp://waku.local')).toThrow('ws:// or wss://');
    expect(() => normalizeDaemonAddress('ws://user:secret@waku.local')).toThrow('no credentials');
  });

  test('derives a useful default name without losing timestamps', () => {
    const profile = normalizeDaemonProfile(
      { name: '', address: 'wss://work.example.com', token: 'secret' },
      undefined,
      'daemon-id',
      100,
    );
    expect(profile).toEqual({
      id: 'daemon-id',
      name: 'work.example.com',
      address: 'wss://work.example.com',
      createdAt: 100,
      updatedAt: 100,
      lastConnectedAt: null,
    });
    expect(displayHost('ws://10.0.0.4:34123/v1')).toBe('10.0.0.4:34123');
  });

  test('identifies LAN and tailnet addresses', () => {
    expect(isPrivateDaemonAddress('ws://192.168.1.8:34123')).toBe(true);
    expect(isPrivateDaemonAddress('ws://100.100.12.8:34123')).toBe(true);
    expect(isPrivateDaemonAddress('ws://workstation:34123')).toBe(true);
    expect(isPrivateDaemonAddress('ws://[::1]:34123')).toBe(true);
    expect(isPrivateDaemonAddress('ws://[2001:db8::8]:34123')).toBe(false);
    expect(isPrivateDaemonAddress('wss://waku.example.com')).toBe(false);
  });

  test('creates compact initials', () => {
    expect(profileInitials('Home Mac')).toBe('HM');
    expect(profileInitials('studio')).toBe('ST');
  });

  test('recovers valid profiles from a partially corrupt registry', () => {
    expect(parseDaemonProfiles([
      {
        id: 'one',
        name: 'Home',
        address: 'home.local:34123',
        createdAt: 1,
        updatedAt: 2,
        lastConnectedAt: null,
      },
      { id: 'broken' },
    ])).toEqual([
      {
        id: 'one',
        name: 'Home',
        address: 'ws://home.local:34123',
        createdAt: 1,
        updatedAt: 2,
        lastConnectedAt: null,
      },
    ]);
  });
});
