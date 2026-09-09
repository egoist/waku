import { describe, expect, test } from 'bun:test';
import type { DaemonSettings } from '@waku/client';

import {
  isProviderDisabled,
  orderedProviderToggles,
  withComputerUse,
  withProviderDisabled,
} from './daemon-settings';

function settings(overrides: Partial<DaemonSettings> = {}): DaemonSettings {
  return {
    computer_use_enabled: false,
    computer_use_allowed_apps: [],
    disabled_providers: [],
    provider_binary_overrides: {},
    ...overrides,
  };
}

describe('daemon settings', () => {
  test('treats a missing settings object as fully enabled', () => {
    expect(isProviderDisabled(undefined, 'codex')).toBe(false);
    expect(isProviderDisabled(settings(), 'codex')).toBe(false);
  });

  test('disables and re-enables a provider', () => {
    const disabled = withProviderDisabled(settings(), 'codex', true);
    expect(disabled.disabled_providers).toEqual(['codex']);
    expect(isProviderDisabled(disabled, 'codex')).toBe(true);

    const enabled = withProviderDisabled(disabled, 'codex', false);
    expect(enabled.disabled_providers).toEqual([]);
  });

  test('does not duplicate a provider disabled twice', () => {
    const once = withProviderDisabled(settings(), 'codex', true);
    expect(withProviderDisabled(once, 'codex', true).disabled_providers).toEqual(['codex']);
  });

  test('leaves every other field untouched', () => {
    const source = settings({
      computer_use_enabled: true,
      provider_binary_overrides: { codex: '/usr/local/bin/codex' },
    });
    const next = withProviderDisabled(source, 'claude', true);
    expect(next.computer_use_enabled).toBe(true);
    expect(next.provider_binary_overrides).toEqual({ codex: '/usr/local/bin/codex' });
  });

  test('orders toggles by the given provider list, not the daemon order', () => {
    const toggles = orderedProviderToggles(
      settings({ disabled_providers: ['codex'] }),
      ['claude', 'codex', 'pi'],
    );
    expect(toggles).toEqual([
      { provider: 'claude', disabled: false },
      { provider: 'codex', disabled: true },
      { provider: 'pi', disabled: false },
    ]);
  });

  test('switches computer use without disturbing providers', () => {
    const source = settings({ disabled_providers: ['amp'] });
    const next = withComputerUse(source, true);
    expect(next.computer_use_enabled).toBe(true);
    expect(next.disabled_providers).toEqual(['amp']);
  });
});
