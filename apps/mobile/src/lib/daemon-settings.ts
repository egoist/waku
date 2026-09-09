import type { DaemonSettings, ProviderKind } from '@waku/client';

export function isProviderDisabled(
  settings: DaemonSettings | undefined,
  provider: ProviderKind,
): boolean {
  return settings?.disabled_providers.includes(provider) ?? false;
}

/** Enable or disable an agent. The daemon owns the list, so this returns the
 * whole settings object to send back rather than patching in place. */
export function withProviderDisabled(
  settings: DaemonSettings,
  provider: ProviderKind,
  disabled: boolean,
): DaemonSettings {
  const without = settings.disabled_providers.filter((item) => item !== provider);
  return {
    ...settings,
    disabled_providers: disabled ? [...without, provider] : without,
  };
}

/** Toggle ordering follows a stable provider list, not the daemon's, so the
 * screen does not reshuffle when a provider is switched off. */
export function orderedProviderToggles(
  settings: DaemonSettings | undefined,
  providers: ProviderKind[],
): Array<{ provider: ProviderKind; disabled: boolean }> {
  return providers.map((provider) => ({
    provider,
    disabled: isProviderDisabled(settings, provider),
  }));
}

/** Computer Use is a daemon-host capability; the phone can switch it but not
 * enumerate what it grants. */
export function withComputerUse(
  settings: DaemonSettings,
  enabled: boolean,
): DaemonSettings {
  return { ...settings, computer_use_enabled: enabled };
}
