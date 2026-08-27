import { useQueries, useQuery } from '@tanstack/react-query'
import type { ProviderInstance, ProviderKind } from '@waku/client'
import { useDaemon } from '@/lib/daemon-context'
import {
  daemonKeys,
  discoverComposerCommands,
  hydrateSession,
  inspectWorkspaceBranches,
  listSessionTurnRefs,
  listComposerFiles,
  loadComposerDrafts,
  loadSkills,
  loadDaemonSettings,
  loadTaskState,
  loadUsageHistory,
  probeProvider,
} from '@/lib/daemon-api'
import {
  browserProviderProbeStorage,
  PROVIDER_PROBE_CACHE_STALE_TIME,
  readProviderProbeCache,
  writeProviderProbeCache,
  type ProviderProbeResult,
} from '@/lib/provider-probe-cache'
import { providerInstance, providerInstances } from '@/lib/provider-instances'

export function useTaskState() {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.taskState(config?.address ?? 'disconnected'),
    queryFn: () => loadTaskState(requireClient(client)),
    enabled: phase === 'connected' && Boolean(client && config),
  })
}

export function useComposerDrafts() {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.composerDrafts(config?.address ?? 'disconnected'),
    queryFn: () => loadComposerDrafts(requireClient(client)),
    enabled: phase === 'connected' && Boolean(client && config),
    staleTime: Number.POSITIVE_INFINITY,
  })
}

export function useWorkspaceBranches(cwd: string | undefined) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.workspace(config?.address ?? 'disconnected', cwd ?? 'none'),
    queryFn: () => inspectWorkspaceBranches(requireClient(client), cwd!),
    enabled: phase === 'connected' && Boolean(client && config && cwd),
    staleTime: 5_000,
  })
}

export function useSessionTurnRefs(
  cwd: string | undefined,
  sessionId: string | undefined,
) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.sessionTurnRefs(
      config?.address ?? 'disconnected',
      cwd ?? 'none',
      sessionId ?? 'none',
    ),
    queryFn: () => listSessionTurnRefs(requireClient(client), cwd!, sessionId!),
    enabled: phase === 'connected' && Boolean(client && config && cwd && sessionId),
    staleTime: 5_000,
  })
}

export function useComposerFiles(cwd: string | undefined) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.composerFiles(config?.address ?? 'disconnected', cwd ?? 'none'),
    queryFn: () => listComposerFiles(requireClient(client), cwd!),
    enabled: phase === 'connected' && Boolean(client && config && cwd),
    staleTime: Number.POSITIVE_INFINITY,
  })
}

export function useComposerCommands(
  provider: ProviderKind | undefined,
  providerInstanceId: string | null | undefined,
  cwd: string | undefined,
) {
  const { client, config, phase } = useDaemon()
  const settings = useDaemonSettings()
  const instance = settings.data && provider
    ? providerInstance(settings.data, provider, providerInstanceId)
    : null
  const binaryOverride = instance?.binaryOverride ?? null
  const instanceId = instance?.id ?? provider ?? 'codex'
  return useQuery({
    queryKey: daemonKeys.slashCommands(
      config?.address ?? 'disconnected',
      provider ?? 'codex',
      instanceId,
      cwd ?? 'none',
      binaryOverride,
    ),
    queryFn: () => discoverComposerCommands(
      requireClient(client),
      provider!,
      providerInstanceId ?? null,
      cwd!,
      binaryOverride,
    ),
    enabled: phase === 'connected' && Boolean(
      client && config && provider && cwd && settings.data,
    ),
    staleTime: Number.POSITIVE_INFINITY,
  })
}

export function useSession(sessionId: string | undefined) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.session(config?.address ?? 'disconnected', sessionId ?? 'none'),
    queryFn: () => hydrateSession(requireClient(client), sessionId!),
    enabled: phase === 'connected' && Boolean(client && config && sessionId),
    staleTime: 1_000,
  })
}

export function useDaemonSettings() {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.settings(config?.address ?? 'disconnected'),
    queryFn: () => loadDaemonSettings(requireClient(client)),
    enabled: phase === 'connected' && Boolean(client && config),
    staleTime: 60_000,
  })
}

export function useProviderProbe(
  provider: ProviderKind | undefined,
  providerInstanceId?: string | null,
) {
  const { client, config, phase } = useDaemon()
  const settings = useDaemonSettings()
  const address = config?.address ?? 'disconnected'
  const identity = providerInstanceId ?? provider
  const instance = settings.data && provider
    ? providerInstance(settings.data, provider, providerInstanceId)
    : undefined
  const cached = config && identity
    ? readProviderProbeCache(browserProviderProbeStorage(), address, identity)
    : undefined
  const binaryOverride = instance
    ? instance.binaryOverride ?? null
    : cached?.binaryOverride ?? null
  const initial = cached?.binaryOverride === binaryOverride ? cached : undefined
  return useQuery({
    queryKey: daemonKeys.provider(
      address,
      identity ?? 'codex',
      binaryOverride,
    ),
    queryFn: async () => {
      const data = await probeProvider(
        requireClient(client),
        provider!,
        settings.data!,
        {},
        providerInstanceId,
      )
      writeProviderProbeCache(
        browserProviderProbeStorage(),
        address,
        identity!,
        binaryOverride,
        data,
      )
      return data
    },
    enabled:
      phase === 'connected' &&
      Boolean(client && config && provider && identity && settings.data),
    initialData: initial?.data,
    initialDataUpdatedAt: initial?.updatedAt,
    staleTime: PROVIDER_PROBE_CACHE_STALE_TIME,
  })
}

export function useProviderProbes(enabled = true) {
  const { client, config, phase } = useDaemon()
  const settings = useDaemonSettings()
  const address = config?.address ?? 'disconnected'
  const storage = browserProviderProbeStorage()
  const active = enabled && phase === 'connected' && Boolean(client && config && settings.data)
  const instances = settings.data ? providerInstances(settings.data) : []
  const queries = useQueries({
    queries: instances.map((instance) => {
      const cached = config ? readProviderProbeCache(storage, address, instance.id) : undefined
      const binaryOverride = instance.binaryOverride ?? null
      const initial = cached?.binaryOverride === binaryOverride ? cached : undefined
      return {
        queryKey: daemonKeys.provider(address, instance.id, binaryOverride),
        queryFn: async () => {
          const data = await probeProvider(
            requireClient(client),
            instance.provider,
            settings.data!,
            {},
            instance.id === instance.provider ? null : instance.id,
          )
          writeProviderProbeCache(storage, address, instance.id, binaryOverride, data)
          return data
        },
        enabled: active,
        initialData: initial?.data,
        initialDataUpdatedAt: initial?.updatedAt,
        staleTime: PROVIDER_PROBE_CACHE_STALE_TIME,
      }
    }),
  })
  return collectProviderQueries(queries, instances)
}

export function useProviderDetections(enabled = true) {
  const { client, config, phase } = useDaemon()
  const settings = useDaemonSettings()
  const active = enabled && phase === 'connected' && Boolean(client && config && settings.data)
  const instances = settings.data ? providerInstances(settings.data) : []
  const queries = useQueries({
    queries: instances.map((instance) => ({
      queryKey: daemonKeys.providerDetection(
        config?.address ?? 'disconnected',
        instance.id,
      ),
      queryFn: () => probeProvider(
        requireClient(client),
        instance.provider,
        settings.data!,
        { discoverModels: false, probeVersion: false },
        instance.id === instance.provider ? null : instance.id,
      ),
      enabled: active,
      staleTime: 60_000,
    })),
  })
  return collectProviderQueries(queries, instances)
}

export function useSkills(projects: Parameters<typeof loadSkills>[1]) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.skills(config?.address ?? 'disconnected'),
    queryFn: () => loadSkills(requireClient(client), projects),
    enabled: phase === 'connected' && Boolean(client && config),
  })
}

export function useUsageHistory(
  window: Parameters<typeof loadUsageHistory>[1],
  projects: Parameters<typeof loadUsageHistory>[2],
) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.usage(config?.address ?? 'disconnected', window),
    queryFn: () => loadUsageHistory(requireClient(client), window, projects),
    enabled: phase === 'connected' && Boolean(client && config),
    placeholderData: (previous) => previous,
  })
}

function requireClient<T>(client: T | null): T {
  if (!client) throw new Error('Waku daemon is disconnected')
  return client
}

function collectProviderQueries(
  queries: Array<{
    data?: ProviderProbeResult
    dataUpdatedAt: number
    error: unknown
    isFetching: boolean
    isPending: boolean
  }>,
  instances: ProviderInstance[],
) {
  const data: Partial<Record<string, ProviderProbeResult>> = {}
  const states: Record<string, {
    dataUpdatedAt: number
    error: unknown
    isPending: boolean
  }> = {}
  instances.forEach((instance, index) => {
    const query = queries[index]!
    if (query.data) data[instance.id] = query.data
    states[instance.id] = {
      dataUpdatedAt: query.dataUpdatedAt,
      error: query.error,
      isPending: query.isPending,
    }
  })
  return {
    data,
    states,
    isFetching: queries.some((query) => query.isFetching),
    isPending: queries.some((query) => query.isPending),
  }
}
