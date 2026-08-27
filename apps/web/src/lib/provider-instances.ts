import type { AgentSession, DaemonSettings, ProviderInstance, ProviderKind } from '@waku/client'
import { PROVIDERS } from '@/components/waku-icon'

export function providerInstances(settings: DaemonSettings): ProviderInstance[] {
  const builtins = PROVIDERS.map(({ id, name }) => ({
    id,
    provider: id,
    name,
    enabled: !(settings.disabled_providers ?? []).includes(id),
    binaryOverride: settings.provider_binary_overrides?.[id] ?? null,
    environment: {},
  } satisfies ProviderInstance))
  const seen = new Set<string>(builtins.map((instance) => instance.id))
  return [
    ...builtins,
    ...(settings.custom_provider_instances ?? []).filter((instance) => {
      if (!instance.id.trim() || !instance.name.trim() || seen.has(instance.id)) return false
      seen.add(instance.id)
      return true
    }),
  ]
}

export function providerInstance(
  settings: DaemonSettings,
  provider: ProviderKind,
  providerInstanceId?: string | null,
): ProviderInstance | undefined {
  const identity = providerInstanceId ?? provider
  return providerInstances(settings).find(
    (instance) => instance.provider === provider && instance.id === identity,
  )
}

export function sessionProviderInstanceId(session: AgentSession): string {
  return session.provider_instance_id ?? session.provider
}
