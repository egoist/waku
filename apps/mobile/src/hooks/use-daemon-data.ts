import { useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import type { ProviderKind } from '@waku/client';
import {
  PROVIDER_PROBE_CACHE_STALE_TIME,
  readProviderProbeCache,
  writeProviderProbeCache,
} from '@waku/client/provider-probe-cache';

import {
  daemonKeys,
  hydrateSession,
  loadDaemonSettings,
  loadTaskState,
  probeProvider,
  type TaskState,
} from '@/lib/daemon-api';
import { persistentStorageSync } from '@/lib/composer-preferences-store';
import { useDaemon } from '@/lib/daemon-context';
import { providerLabel } from '@/lib/session-presentation';

const PROVIDERS: ProviderKind[] = [
  'codex',
  'claude',
  'cursor',
  'amp',
  'openCode',
  'grok',
  'kimi',
  'deepSeek',
  'fx',
  'ohMyPi',
  'pi',
];

export function useTaskState() {
  const { activeProfile, client, phase } = useDaemon();
  return useQuery({
    queryKey: daemonKeys.taskState(activeProfile?.id ?? 'disconnected'),
    queryFn: () => loadTaskState(requireClient(client)),
    enabled: phase === 'connected' && Boolean(activeProfile && client),
    staleTime: 1_000,
  });
}

export function useSession(sessionId: string | undefined) {
  const { activeProfile, client, phase } = useDaemon();
  const queryClient = useQueryClient();
  const profileId = activeProfile?.id ?? 'disconnected';
  return useQuery({
    queryKey: daemonKeys.session(
      profileId,
      sessionId ?? 'missing',
    ),
    queryFn: () => hydrateSession(requireClient(client), sessionId!),
    enabled: phase === 'connected' && Boolean(activeProfile && client && sessionId),
    placeholderData: () => queryClient
      .getQueryData<TaskState>(daemonKeys.taskState(profileId))
      ?.sessions.find((session) => session.id === sessionId),
    staleTime: 1_000,
  });
}

export function useDaemonSettings() {
  const { activeProfile, client, phase } = useDaemon();
  return useQuery({
    queryKey: daemonKeys.settings(activeProfile?.id ?? 'disconnected'),
    queryFn: () => loadDaemonSettings(requireClient(client)),
    enabled: phase === 'connected' && Boolean(activeProfile && client),
    staleTime: 60_000,
  });
}

/** Web's probe-query recipe: seed from the persistent probe cache so model
 * lists render instantly across app restarts, refresh after the shared 24h
 * staleness, and write fresh probes back through the same cache. */
function providerModelsQuery(
  daemon: ReturnType<typeof useDaemon>,
  settings: ReturnType<typeof useDaemonSettings>,
  provider: ProviderKind,
) {
  const address = daemon.activeProfile?.address ?? 'disconnected';
  const cached = daemon.activeProfile
    ? readProviderProbeCache(persistentStorageSync(), address, provider)
    : undefined;
  const binaryOverride = settings.data
    ? settings.data.provider_binary_overrides?.[provider] ?? null
    : cached?.binaryOverride ?? null;
  const initial = cached && cached.binaryOverride === binaryOverride ? cached : undefined;
  return {
    queryKey: [
      ...daemonKeys.provider(daemon.activeProfile?.id ?? 'disconnected', provider),
      'models',
    ],
    queryFn: async () => {
      const data = await probeProvider(requireClient(daemon.client), provider, settings.data!, {
        discoverModels: true,
        probeVersion: false,
      });
      writeProviderProbeCache(persistentStorageSync(), address, provider, binaryOverride, data);
      return data;
    },
    enabled: daemon.phase === 'connected' &&
      Boolean(daemon.activeProfile && daemon.client && settings.data),
    initialData: initial?.data,
    initialDataUpdatedAt: initial?.updatedAt,
    staleTime: PROVIDER_PROBE_CACHE_STALE_TIME,
  };
}

/** Full probe with model discovery, for the model picker. Screens mount this
 * ahead of opening the sheet so the list is warm by the time it appears. */
export function useProviderModels(provider: ProviderKind | null) {
  const daemon = useDaemon();
  const settings = useDaemonSettings();
  return useQuery(providerModelsQuery(daemon, settings, provider ?? 'codex'));
}

/** Model discovery across every given provider, cache-shared with
 * useProviderModels. Backs the cross-provider model picker. */
export function useAllProviderModels(providers: ProviderKind[]) {
  const daemon = useDaemon();
  const settings = useDaemonSettings();
  const queries = useQueries({
    queries: providers.map((provider) => providerModelsQuery(daemon, settings, provider)),
  });
  return providers.map((id, index) => ({
    id,
    label: providerLabel(id),
    models: queries[index]?.data?.models ?? [],
    isPending: queries[index]?.isPending ?? true,
  }));
}

export function useProviderCatalog() {
  const { activeProfile, client, phase } = useDaemon();
  const settings = useDaemonSettings();
  const enabledProviders = PROVIDERS.filter((provider) => (
    !settings.data?.disabled_providers.includes(provider)
  ));
  const queries = useQueries({
    queries: PROVIDERS.map((provider) => {
      // Seed installed/path detection from the persistent probe cache so the
      // catalog renders instantly on a cold start; the fresh light probe
      // still revalidates. The cache is only written by model discovery.
      const cached = activeProfile
        ? readProviderProbeCache(persistentStorageSync(), activeProfile.address, provider)
        : undefined;
      return {
        queryKey: daemonKeys.provider(activeProfile?.id ?? 'disconnected', provider),
        queryFn: () => probeProvider(requireClient(client), provider, settings.data!, {
          discoverModels: false,
          probeVersion: false,
        }),
        enabled:
          phase === 'connected' &&
          Boolean(activeProfile && client && settings.data) &&
          enabledProviders.includes(provider),
        initialData: cached?.data,
        initialDataUpdatedAt: cached?.updatedAt,
        staleTime: 60_000,
      };
    }),
  });
  return {
    providers: enabledProviders.map((id) => {
      const query = queries[PROVIDERS.indexOf(id)]!;
      return {
        id,
        label: providerLabel(id),
        installed: query.data?.installed === true,
        path: query.data?.path ?? null,
        isPending: query.isPending,
        error: query.error,
      };
    }),
    isPending: settings.isPending || queries.some((query) => query.isPending && query.fetchStatus !== 'idle'),
    error: settings.error,
  };
}

function requireClient(client: ReturnType<typeof useDaemon>['client']) {
  if (!client) throw new Error('Waku daemon is disconnected');
  return client;
}
