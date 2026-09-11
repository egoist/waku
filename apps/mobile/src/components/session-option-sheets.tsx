import { BottomSheetFlatList } from '@expo/ui/community/bottom-sheet';
import type {
  AgentSession,
  ProviderKind,
  ProviderModel,
  ProviderSessionSummary,
  RuntimeMode,
} from '@waku/client';
import * as Haptics from 'expo-haptics';
import { useEffect, useMemo, useState } from 'react';
import {
  ActivityIndicator,
  StyleSheet,
  Text,
  TextInput,
  View,
  useWindowDimensions,
} from 'react-native';
import Animated, {
  Easing,
  useAnimatedStyle,
  useReducedMotion,
  useSharedValue,
  withTiming,
} from 'react-native-reanimated';

import { AppSymbol } from './app-symbol';
import { ProviderIcon } from './provider-icon';
import { Sheet, SheetRow } from './sheet';
import { AppPressable } from '@/components/app-pressable';

import { NativeTint, Radius } from '@/constants/theme';
import { useAllProviderModels, useProviderModels } from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import {
  resolveModelTraitSelection,
  type ModelTraitSelection,
} from '@/lib/model-traits';
import {
  providerLabel,
  runtimeModeLabel,
  type TurnOption,
} from '@/lib/session-presentation';
import { listProviderSessions, providerSessionNativeId } from '@/lib/daemon-api';
import { useDaemon } from '@/lib/daemon-context';
import { useRuntime } from '@/lib/runtime-context';

export interface ModelSelection {
  model: string | null;
  reasoningEffort: string | null;
}

export interface AgentPresetSelection {
  agentPreset: string | null;
}

/** Picks how many turns a rewind or a fork keeps.
 *
 * Rewind and fork are destructive and irreversible, so the sheet only selects
 * a target — the caller confirms before anything is sent to the daemon.
 */
export function TurnSheet({
  visible,
  onDismiss,
  title,
  note,
  turns,
  onPick,
}: {
  visible: boolean;
  onDismiss: () => void;
  title: string;
  note?: string;
  turns: TurnOption[];
  onPick: (turnCount: number) => void;
}) {
  const theme = useTheme();

  function pick(turnCount: number) {
    void Haptics.selectionAsync();
    onPick(turnCount);
    onDismiss();
  }

  return (
    <Sheet onDismiss={onDismiss} title={title} visible={visible}>
      {note ? (
        <Text style={[styles.note, { color: theme.textTertiary }]}>{note}</Text>
      ) : null}
      {turns.map((turn) => (
        <SheetRow
          key={turn.turnCount}
          label={turn.label}
          onPress={() => pick(turn.turnCount)}
        />
      ))}
      {!turns.length && (
        <Text style={[styles.note, { color: theme.textTertiary }]}>
          This task has no completed turn to rewind to yet.
        </Text>
      )}
    </Sheet>
  );
}

export function modelDisplayName(
  models: ProviderModel[] | undefined,
  model: string | null,
): string {
  if (!model) return 'Default model';
  return models?.find((item) => item.id === model)?.name ?? model;
}

/** Model + reasoning-effort picker, backed by the daemon's model discovery. */
export function ModelSheet({
  visible,
  onDismiss,
  provider,
  model,
  reasoningEffort,
  onApply,
}: {
  visible: boolean;
  onDismiss: () => void;
  provider: ProviderKind;
  model: string | null;
  reasoningEffort: string | null;
  onApply: (selection: ModelSelection) => void;
}) {
  const theme = useTheme();
  const probe = useProviderModels(visible ? provider : null);
  const models = probe.data?.models ?? [];
  const selected = model ? models.find((item) => item.id === model) : models.find((item) => item.is_default);
  const efforts = selected?.reasoning_efforts ?? [];

  function pickModel(next: ProviderModel) {
    void Haptics.selectionAsync();
    onApply({
      model: next.id,
      reasoningEffort: next.default_reasoning_effort ?? null,
    });
    if (!next.reasoning_efforts.length) onDismiss();
  }

  return (
    <Sheet onDismiss={onDismiss} title={`${providerLabel(provider)} model`} visible={visible}>
      {probe.isPending ? (
        <View style={styles.loading}>
          <ActivityIndicator color={theme.textTertiary} />
        </View>
      ) : probe.error ? (
        <Text style={[styles.note, { color: theme.danger }]}>
          {probe.error instanceof Error ? probe.error.message : String(probe.error)}
        </Text>
      ) : (
        <>
          {models.map((item) => (
            <SheetRow
              description={item.sub_provider ?? undefined}
              key={item.id}
              label={item.name}
              onPress={() => pickModel(item)}
              selected={model === item.id || (!model && item.is_default)}
            />
          ))}
          {!models.length && (
            <Text style={[styles.note, { color: theme.textTertiary }]}>
              This agent doesn’t expose a model list; it will use its own default.
            </Text>
          )}
          {efforts.length ? (
            <>
              <Text style={[styles.sectionTitle, { color: theme.textSecondary }]}>
                REASONING EFFORT
              </Text>
              {efforts.map((effort) => (
                <SheetRow
                  description={effort.description ?? undefined}
                  key={effort.id}
                  label={effort.label}
                  onPress={() => {
                    void Haptics.selectionAsync();
                    onApply({ model: selected?.id ?? model, reasoningEffort: effort.id });
                    onDismiss();
                  }}
                  selected={(reasoningEffort ?? selected?.default_reasoning_effort) === effort.id}
                />
              ))}
            </>
          ) : null}
        </>
      )}
    </Sheet>
  );
}

/** All model-advertised options in one sheet. Unlike single-choice pickers,
 * it stays open after a choice so effort, tier, and context can be configured
 * together before starting a task. */
export function ModelTraitsSheet({
  visible,
  onDismiss,
  model,
  selection,
  onApply,
}: {
  visible: boolean;
  onDismiss: () => void;
  model: ProviderModel;
  selection: ModelTraitSelection;
  onApply: (changes: Partial<ModelTraitSelection>) => void;
}) {
  const theme = useTheme();
  const resolved = resolveModelTraitSelection(model, selection);

  function pick(changes: Partial<ModelTraitSelection>) {
    void Haptics.selectionAsync();
    onApply(changes);
  }

  return (
    <Sheet onDismiss={onDismiss} visible={visible}>
      {model.reasoning_efforts.length ? (
        <>
          <Text style={[styles.sectionTitle, { color: theme.textSecondary }]}>
            REASONING EFFORT
          </Text>
          {model.reasoning_efforts.map((option) => (
            <SheetRow
              description={optionDescription(
                option.description,
                model.default_reasoning_effort === option.id,
              )}
              key={option.id}
              label={option.label}
              onPress={() => pick({ reasoningEffort: option.id })}
              selected={resolved.reasoningEffort === option.id}
            />
          ))}
        </>
      ) : null}
      {model.service_tiers.length ? (
        <>
          <Text style={[styles.sectionTitle, { color: theme.textSecondary }]}>
            SERVICE TIER
          </Text>
          <SheetRow
            description={(model.default_service_tier ?? 'default') === 'default'
              ? 'Default'
              : undefined}
            label="Standard"
            onPress={() => pick({ serviceTier: 'default' })}
            selected={resolved.serviceTier === 'default'}
          />
          {model.service_tiers.map((option) => (
            <SheetRow
              description={optionDescription(
                option.description,
                model.default_service_tier === option.id,
              )}
              key={option.id}
              label={option.label}
              onPress={() => pick({ serviceTier: option.id })}
              selected={resolved.serviceTier === option.id}
            />
          ))}
        </>
      ) : null}
      {model.context_windows.length ? (
        <>
          <Text style={[styles.sectionTitle, { color: theme.textSecondary }]}>
            CONTEXT WINDOW
          </Text>
          {model.context_windows.map((option) => (
            <SheetRow
              description={optionDescription(
                option.description,
                model.default_context_window === option.id,
              )}
              key={option.id}
              label={option.label}
              onPress={() => pick({ contextWindow: option.id })}
              selected={resolved.contextWindow === option.id}
            />
          ))}
        </>
      ) : null}
    </Sheet>
  );
}

export interface ProviderModelSelection extends ModelTraitSelection {
  provider: ProviderKind;
  model: string | null;
}

/**
 * Two-screen cross-provider model picker: opening lands on the current
 * provider's models with a search filter over a virtualized list; the back
 * row (named after the provider) slides across to the providers screen, and
 * choosing a provider slides back into that provider's models.
 */
export function ModelPickerSheet({
  visible,
  onDismiss,
  providers,
  provider,
  model,
  onApply,
}: {
  visible: boolean;
  onDismiss: () => void;
  providers: ProviderKind[];
  provider: ProviderKind | null;
  model: string | null;
  onApply: (selection: ProviderModelSelection) => void;
}) {
  const theme = useTheme();
  const { height: windowHeight } = useWindowDimensions();
  const catalog = useAllProviderModels(visible ? providers : []);
  const [browsing, setBrowsing] = useState<ProviderKind | null>(provider);
  const [search, setSearch] = useState('');
  const reduceMotion = useReducedMotion();
  // 0 = providers page, 1 = models page.
  const progress = useSharedValue(1);
  const pageWidth = useSharedValue(0);
  const slideStyle = useAnimatedStyle(() => ({
    transform: [{ translateX: -pageWidth.value * progress.value }],
  }));

  useEffect(() => {
    if (!visible) return;
    const initial = provider ?? providers[0] ?? null;
    setBrowsing(initial);
    setSearch('');
    progress.value = initial ? 1 : 0;
  }, [progress, provider, providers, visible]);

  function slideTo(target: 0 | 1) {
    progress.value = reduceMotion
      ? target
      : withTiming(target, { duration: 260, easing: Easing.bezier(0.32, 0.72, 0.25, 1) });
  }

  const entry = catalog.find((item) => item.id === browsing);
  const preferredModelId = entry?.models.find((item) => item.is_default)?.id
    ?? entry?.models[0]?.id;
  const listHeight = Math.round(windowHeight * 0.48);
  const items = useMemo<ProviderModel[]>(() => {
    const query = search.trim().toLocaleLowerCase();
    const models = entry?.models ?? [];
    if (!query) return models;
    return models.filter((item) => (
      item.name.toLocaleLowerCase().includes(query) ||
        item.id.toLocaleLowerCase().includes(query) ||
        item.sub_provider?.toLocaleLowerCase().includes(query)
    ));
  }, [entry?.models, search]);

  function pickModel(next: ProviderModel) {
    if (!browsing) return;
    void Haptics.selectionAsync();
    onApply({
      provider: browsing,
      model: next.id,
      reasoningEffort: next.default_reasoning_effort ?? null,
      serviceTier: next.default_service_tier ?? null,
      contextWindow: next.default_context_window ?? null,
    });
    onDismiss();
  }

  return (
    <Sheet onDismiss={onDismiss} scrollable={false} visible={visible}>
      <View
        style={styles.pagerClip}
        onLayout={(event) => {
          pageWidth.value = event.nativeEvent.layout.width;
        }}>
        <Animated.View style={[styles.pagerTrack, slideStyle]}>
          <View style={styles.page}>
            <BottomSheetFlatList
              data={providers}
              keyExtractor={(id) => id}
              keyboardShouldPersistTaps="handled"
              renderItem={({ item: id }) => (
                <SheetRow
                  label={providerLabel(id)}
                  leading={<ProviderIcon provider={id} size={20} />}
                  onPress={() => {
                    void Haptics.selectionAsync();
                    setBrowsing(id);
                    setSearch('');
                    slideTo(1);
                  }}
                  selected={id === provider}
                />
              )}
              showsVerticalScrollIndicator={false}
              style={{ height: listHeight }}
              ListEmptyComponent={(
                <Text style={[styles.note, { color: theme.textTertiary }]}>
                  No agents are installed on this daemon host.
                </Text>
              )}
            />
          </View>
          <View style={styles.page}>
            <AppPressable
              accessibilityHint="Shows all providers"
              accessibilityLabel={browsing ? providerLabel(browsing) : 'Provider'}
              accessibilityRole="button"
              onPress={() => slideTo(0)}
              style={({ pressed }) => [styles.backRow, { opacity: pressed ? 0.55 : 1 }]}>
              <AppSymbol
                name={{ ios: 'chevron.left', android: 'arrow_back', web: 'arrow_back' }}
                size={14}
                tintColor={NativeTint}
              />
              <Text style={[styles.backLabel, { color: NativeTint }]}>
                {browsing ? providerLabel(browsing) : 'Provider'}
              </Text>
            </AppPressable>
            <View style={[styles.searchField, { backgroundColor: theme.overlayStrong }]}>
              <AppSymbol
                name={{ ios: 'magnifyingglass', android: 'search', web: 'search' }}
                size={14}
                tintColor={theme.textTertiary}
              />
              <TextInput
                accessibilityLabel="Search models"
                autoCapitalize="none"
                autoCorrect={false}
                placeholder="Search models"
                placeholderTextColor={theme.textTertiary}
                selectionColor={NativeTint}
                style={[styles.searchInput, { color: theme.text }]}
                value={search}
                onChangeText={setSearch}
              />
              {search.length > 0 && (
                <AppPressable
                  accessibilityLabel="Clear search"
                  accessibilityRole="button"
                  hitSlop={8}
                  onPress={() => setSearch('')}
                  style={({ pressed }) => ({ opacity: pressed ? 0.5 : 1 })}>
                  <AppSymbol
                    name={{ ios: 'xmark.circle.fill', android: 'cancel', web: 'cancel' }}
                    size={15}
                    tintColor={theme.textTertiary}
                  />
                </AppPressable>
              )}
            </View>
            {entry?.isPending ? (
              <View style={[styles.loading, { height: listHeight }]}>
                <ActivityIndicator color={theme.textTertiary} />
              </View>
            ) : (
              <BottomSheetFlatList
                data={items}
                initialNumToRender={14}
                keyExtractor={(item) => item.id}
                keyboardShouldPersistTaps="handled"
                renderItem={({ item }) => (
                  <SheetRow
                    description={item.sub_provider ?? undefined}
                    label={item.name}
                    onPress={() => pickModel(item)}
                    selected={provider === browsing &&
                      (model === item.id || (!model && item.id === preferredModelId))}
                  />
                )}
                showsVerticalScrollIndicator={false}
                style={{ height: listHeight }}
                ListEmptyComponent={(
                  <Text style={[styles.note, { color: theme.textTertiary }]}>
                    {search.trim()
                      ? 'No models match your search.'
                      : 'This agent doesn’t expose a model list; it will use its own default.'}
                  </Text>
                )}
              />
            )}
          </View>
        </Animated.View>
      </View>
    </Sheet>
  );
}

const ACCESS_MODES: Array<{ id: RuntimeMode; description: string }> = [
  { id: 'ask', description: 'Approve every command and file edit.' },
  { id: 'autoAcceptEdits', description: 'Edits apply automatically; commands still ask.' },
  { id: 'auto', description: 'Works autonomously inside the project.' },
  { id: 'fullAccess', description: 'No approval prompts. The agent acts freely.' },
];

/** Agent preset picker for providers that expose startable compositions
 * (DeepSeek Harness, OpenCode v1). Mirrors the desktop agent chip. */
export function AgentPresetSheet({
  visible,
  onDismiss,
  provider,
  agentPreset,
  onApply,
}: {
  visible: boolean;
  onDismiss: () => void;
  provider: ProviderKind;
  agentPreset: string | null;
  onApply: (selection: AgentPresetSelection) => void;
}) {
  const theme = useTheme();
  const probe = useProviderModels(visible ? provider : null);
  const presets = probe.data?.agent_presets ?? [];
  const fallback = presets.find((preset) => preset.is_default) ?? presets[0];

  return (
    <Sheet onDismiss={onDismiss} title={`${providerLabel(provider)} agent`} visible={visible}>
      {probe.isPending ? (
        <View style={styles.loading}>
          <ActivityIndicator color={theme.textTertiary} />
        </View>
      ) : probe.error ? (
        <Text style={[styles.note, { color: theme.danger }]}>
          {probe.error instanceof Error ? probe.error.message : String(probe.error)}
        </Text>
      ) : (
        <>
          {presets.map((preset) => (
            <SheetRow
              description={preset.description ?? undefined}
              key={preset.id}
              label={preset.name}
              onPress={() => {
                void Haptics.selectionAsync();
                onApply({ agentPreset: preset.id });
                onDismiss();
              }}
              selected={agentPreset === preset.id || (!agentPreset && preset.id === fallback?.id)}
            />
          ))}
          {!presets.length && (
            <Text style={[styles.note, { color: theme.textTertiary }]}>
              This agent doesn’t expose any startable presets.
            </Text>
          )}
        </>
      )}
    </Sheet>
  );
}

/** Access-mode picker mirroring the desktop composer's access control. */
export function AccessSheet({
  visible,
  onDismiss,
  mode,
  onApply,
}: {
  visible: boolean;
  onDismiss: () => void;
  mode: RuntimeMode;
  onApply: (mode: RuntimeMode) => void;
}) {
  return (
    <Sheet onDismiss={onDismiss} title="Agent access" visible={visible}>
      {ACCESS_MODES.map((item) => (
        <SheetRow
          description={item.description}
          key={item.id}
          label={runtimeModeLabel(item.id)}
          onPress={() => {
            void Haptics.selectionAsync();
            onApply(item.id);
            onDismiss();
          }}
          selected={mode === item.id}
        />
      ))}
    </Sheet>
  );
}

function optionDescription(description: string | null | undefined, isDefault: boolean) {
  if (description && isDefault) return `Default · ${description}`;
  return description ?? (isDefault ? 'Default' : undefined);
}

/** Adopts a session an agent CLI started on the daemon host as a Waku task.
 *
 * The sheet first lists installed providers, then the external sessions for
 * the chosen one. Picking a session hands the imported task back through
 * `onResume`; the caller navigates to it. */
export function ResumeSessionSheet({
  visible,
  onDismiss,
  onResume,
  installedProviders,
  initialProvider,
}: {
  visible: boolean;
  onDismiss: () => void;
  onResume: (session: AgentSession) => void;
  installedProviders: ProviderKind[];
  initialProvider: ProviderKind | null;
}) {
  const theme = useTheme();
  const daemon = useDaemon();
  const runtime = useRuntime();
  const [provider, setProvider] = useState<ProviderKind | null>(initialProvider);
  const [summaries, setSummaries] = useState<ProviderSessionSummary[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [importing, setImporting] = useState<string | null>(null);

  // Reset to the provider step each time the sheet opens.
  useEffect(() => {
    if (visible) {
      setProvider(initialProvider);
      setSummaries([]);
      setError(null);
      setImporting(null);
    }
  }, [visible, initialProvider]);

  // Listing is keyed to the connection so a reconnect mid-pick refetches rather
  // than leaving a stale empty list.
  useEffect(() => {
    if (!visible || !provider || !daemon.client || daemon.phase !== 'connected') return undefined;
    let current = true;
    setPending(true);
    setError(null);
    setSummaries([]);
    void listProviderSessions(daemon.client, provider)
      .then((sessions) => { if (current) setSummaries(sessions); })
      .catch((cause) => {
        if (current) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => { if (current) setPending(false); });
    return () => { current = false; };
  }, [visible, provider, daemon.client, daemon.phase]);

  async function pick(summary: ProviderSessionSummary) {
    if (importing) return;
    setImporting(providerSessionNativeId(summary.cursor));
    try {
      const session = await runtime.resumeProviderSession(summary);
      onResume(session);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setImporting(null);
    }
  }

  return (
    <Sheet visible={visible} onDismiss={onDismiss} title="Resume external session">
      {provider === null ? (
        installedProviders.map((candidate) => (
          <SheetRow
            key={candidate}
            label={providerLabel(candidate)}
            leading={<ProviderIcon provider={candidate} size={18} />}
            onPress={() => setProvider(candidate)}
          />
        ))
      ) : (
        <>
          <SheetRow
            label="Change provider"
            leading={(
              <AppSymbol
                name={{ ios: 'chevron.left', android: 'arrow_back', web: 'arrow_back' }}
                size={16}
                tintColor={theme.textSecondary}
              />
            )}
            onPress={() => {
              setProvider(null);
              setSummaries([]);
            }}
          />
          {pending ? (
            <View style={styles.loading}>
              <ActivityIndicator color={theme.textTertiary} />
            </View>
          ) : error ? (
            <Text style={[styles.note, { color: theme.danger }]}>{error}</Text>
          ) : summaries.length === 0 ? (
            <Text style={[styles.note, { color: theme.textTertiary }]}>
              No sessions from {providerLabel(provider)} to resume.
            </Text>
          ) : summaries.map((summary) => {
            const id = providerSessionNativeId(summary.cursor);
            return (
              <SheetRow
                key={`${summary.cursor.provider}:${id}`}
                label={summary.title || 'Untitled session'}
                description={`${providerLabel(summary.cursor.provider)} · ${summary.cwd}`}
                disabled={Boolean(importing)}
                onPress={() => void pick(summary)}
              />
            );
          })}
        </>
      )}
    </Sheet>
  );
}

const styles = StyleSheet.create({
  loading: { alignItems: 'center', justifyContent: 'center', paddingVertical: 40 },
  note: { fontSize: 13, lineHeight: 18, paddingHorizontal: 12, paddingVertical: 10 },
  pagerClip: { overflow: 'hidden' },
  pagerTrack: { flexDirection: 'row', width: '200%' },
  page: { paddingTop: 4, width: '50%' },
  backRow: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 5,
    minHeight: 40,
    paddingHorizontal: 10,
  },
  backLabel: { fontSize: 15, fontWeight: '600' },
  searchField: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    flexDirection: 'row',
    gap: 7,
    marginBottom: 8,
    marginHorizontal: 4,
    minHeight: 38,
    paddingHorizontal: 10,
  },
  searchInput: { flex: 1, fontSize: 15, paddingVertical: 7 },
  sectionTitle: {
    fontSize: 12,
    fontWeight: '600',
    letterSpacing: 0.4,
    marginBottom: 6,
    marginHorizontal: 12,
    marginTop: 14,
  },
});
