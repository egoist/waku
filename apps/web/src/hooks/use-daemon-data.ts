import { useQueries, useQuery } from '@tanstack/react-query'
import type { ProviderKind } from '@waku/client'
import { PROVIDERS } from '@/components/waku-icon'
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
  cwd: string | undefined,
) {
  const { client, config, phase } = useDaemon()
  return useQuery({
    queryKey: daemonKeys.slashCommands(
      config?.address ?? 'disconnected',
      provider ?? 'codex',
      cwd ?? 'none',
    ),
    queryFn: () => discoverComposerCommands(requireClient(client), provider!, cwd!),
    enabled: phase === 'connected' && Boolean(client && config && provider && cwd),
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

export function useProviderProbe(provider: ProviderKind | undefined) {
  const { client, config, phase } = useDaemon()
  const settings = useDaemonSettings()
  const address = config?.address ?? 'disconnected'
  const cached = config && provider
    ? readProviderProbeCache(browserProviderProbeStorage(), address, provider)
    : undefined
  const binaryOverride = settings.data && provider
    ? settings.data.provider_binary_overrides?.[provider] ?? null
    : cached?.binaryOverride ?? null
  const initial = cached?.binaryOverride === binaryOverride ? cached : undefined
  return useQuery({
    queryKey: daemonKeys.provider(
      address,
      provider ?? 'codex',
      binaryOverride,
    ),
    queryFn: async () => {
      const data = await probeProvider(requireClient(client), provider!, settings.data!)
      writeProviderProbeCache(
        browserProviderProbeStorage(),
        address,
        provider!,
        binaryOverride,
        data,
      )
      return data
    },
    enabled:
      phase === 'connected' &&
      Boolean(client && config && provider && settings.data),
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
  const queries = useQueries({
    queries: PROVIDERS.map(({ id }) => {
      const cached = config ? readProviderProbeCache(storage, address, id) : undefined
      const binaryOverride = settings.data
        ? settings.data.provider_binary_overrides?.[id] ?? null
        : cached?.binaryOverride ?? null
      const initial = cached?.binaryOverride === binaryOverride ? cached : undefined
      return {
        queryKey: daemonKeys.provider(address, id, binaryOverride),
        queryFn: async () => {
          const data = await probeProvider(requireClient(client), id, settings.data!)
          writeProviderProbeCache(storage, address, id, binaryOverride, data)
          return data
        },
        enabled: active,
        initialData: initial?.data,
        initialDataUpdatedAt: initial?.updatedAt,
        staleTime: PROVIDER_PROBE_CACHE_STALE_TIME,
      }
    }),
  })
  return collectProviderQueries(queries)
}

export function useProviderDetections(enabled = true) {
  const { client, config, phase } = useDaemon()
  const settings = useDaemonSettings()
  const active = enabled && phase === 'connected' && Boolean(client && config && settings.data)
  const queries = useQueries({
    queries: PROVIDERS.map(({ id }) => ({
      queryKey: daemonKeys.providerDetection(config?.address ?? 'disconnected', id),
      queryFn: () => probeProvider(requireClient(client), id, settings.data!, {
        discoverModels: false,
        probeVersion: false,
      }),
      enabled: active,
      staleTime: 60_000,
    })),
  })
  return collectProviderQueries(queries)
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
) {
  const data: Partial<Record<ProviderKind, ProviderProbeResult>> = {}
  const states = {} as Record<ProviderKind, {
    dataUpdatedAt: number
    error: unknown
    isPending: boolean
  }>
  PROVIDERS.forEach(({ id }, index) => {
    const query = queries[index]!
    if (query.data) data[id] = query.data
    states[id] = {
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
