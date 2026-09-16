import { BottomSheetFlatList } from '@expo/ui/community/bottom-sheet';
import type { ProviderKind, ProviderModel, RuntimeMode } from '@waku/client';
import * as Haptics from 'expo-haptics';
import { useEffect, useMemo, useState } from 'react';
import {
  ActivityIndicator,
  Pressable,
  StyleSheet,
  Text,
  TextInput,
  View,
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
import { NativeTint, Radius } from '@/constants/theme';
import { useAllProviderModels, useProviderModels } from '@/hooks/use-daemon-data';
import { useModelSheetListHeight } from '@/hooks/use-model-sheet-list-height';
import { useTheme } from '@/hooks/use-theme';
import {
  resolveModelTraitSelection,
  resolveServiceTier,
  type ModelTraitSelection,
} from '@/lib/model-traits';
import { providerLabel, runtimeModeLabel } from '@/lib/session-presentation';

export interface ModelSelection {
  model: string | null;
  reasoningEffort: string | null;
}

export function modelDisplayName(
  models: ProviderModel[] | undefined,
  model: string | null,
): string {
  if (!model) return 'Default model';
  return models?.find((item) => item.id === model)?.name ?? model;
}

/** Current-provider model picker opened from the task header. */
export function ModelSheet({
  visible,
  onDismiss,
  provider,
  model,
  onApply,
}: {
  visible: boolean;
  onDismiss: () => void;
  provider: ProviderKind;
  model: string | null;
  onApply: (selection: ModelSelection) => void;
}) {
  const theme = useTheme();
  const listHeight = useModelSheetListHeight(visible);
  const probe = useProviderModels(provider);
  const [search, setSearch] = useState('');
  const models = probe.data?.models ?? [];
  const defaultModel = models.find((item) => item.is_default) ?? models[0];
  const items = useMemo(
    () => filterModels(probe.data?.models, search),
    [probe.data?.models, search],
  );

  useEffect(() => {
    if (visible) setSearch('');
  }, [provider, visible]);

  function pickModel(next: ProviderModel) {
    void Haptics.selectionAsync();
    onApply({
      model: next.id,
      reasoningEffort: next.default_reasoning_effort ?? null,
    });
    onDismiss();
  }

  return (
    <Sheet
      onDismiss={onDismiss}
      scrollable={false}
      visible={visible}>
      <ModelSearchField onChangeText={setSearch} value={search} />
      {probe.isPending ? (
        <View style={styles.loading}>
          <ActivityIndicator color={theme.textTertiary} />
        </View>
      ) : probe.error ? (
        <Text style={[styles.note, { color: theme.danger }]}>
          {probe.error instanceof Error ? probe.error.message : String(probe.error)}
        </Text>
      ) : (
        <BottomSheetFlatList
          data={items}
          extraData={model}
          initialNumToRender={14}
          keyExtractor={(item) => item.id}
          keyboardShouldPersistTaps="handled"
          renderItem={({ item }) => (
            <SheetRow
              description={item.sub_provider ?? undefined}
              label={item.name}
              onPress={() => pickModel(item)}
              selected={model === item.id || (!model && defaultModel?.id === item.id)}
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
    </Sheet>
  );
}

/** All model-advertised options in one sheet. Unlike single-choice pickers,
 * it stays open after a choice so effort, tier, and context can be configured
 * together. */
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
      <ServiceTierOptions
        model={model}
        onApply={(tier) => pick({ serviceTier: tier })}
        serviceTier={selection.serviceTier}
      />
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

/** Shared by the new-task and ongoing-task composers. Keep the provider's
 * concrete tier IDs, including `priority` when it advertises Fast that way. */
function ServiceTierOptions({
  model,
  serviceTier,
  onApply,
}: {
  model: ProviderModel;
  serviceTier: string | null;
  onApply: (tier: string) => void;
}) {
  const theme = useTheme();
  if (!model.service_tiers.length) return null;
  const selected = resolveServiceTier(model, serviceTier);

  return (
    <>
      <Text style={[styles.sectionTitle, { color: theme.textSecondary }]}>
        SERVICE TIER
      </Text>
      <SheetRow
        description={(model.default_service_tier ?? 'default') === 'default'
          ? 'Default'
          : undefined}
        label="Standard"
        onPress={() => onApply('default')}
        selected={selected === 'default'}
      />
      {model.service_tiers.map((option) => (
        <SheetRow
          description={optionDescription(
            option.description,
            model.default_service_tier === option.id,
          )}
          key={option.id}
          label={option.label}
          onPress={() => onApply(option.id)}
          selected={selected === option.id}
        />
      ))}
    </>
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
  const listHeight = useModelSheetListHeight(visible);
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
  const items = useMemo(
    () => filterModels(entry?.models, search),
    [entry?.models, search],
  );

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
            <Pressable
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
            </Pressable>
            <ModelSearchField onChangeText={setSearch} value={search} />
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

function filterModels(models: ProviderModel[] | undefined, search: string): ProviderModel[] {
  if (!models) return [];
  const query = search.trim().toLocaleLowerCase();
  if (!query) return models;
  return models.filter((item) => (
    item.name.toLocaleLowerCase().includes(query) ||
      item.id.toLocaleLowerCase().includes(query) ||
      item.sub_provider?.toLocaleLowerCase().includes(query)
  ));
}

function ModelSearchField({
  value,
  onChangeText,
}: {
  value: string;
  onChangeText: (value: string) => void;
}) {
  const theme = useTheme();
  return (
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
        value={value}
        onChangeText={onChangeText}
      />
      {value.length > 0 && (
        <Pressable
          accessibilityLabel="Clear search"
          accessibilityRole="button"
          hitSlop={8}
          onPress={() => onChangeText('')}
          style={({ pressed }) => ({ opacity: pressed ? 0.5 : 1 })}>
          <AppSymbol
            name={{ ios: 'xmark.circle.fill', android: 'cancel', web: 'cancel' }}
            size={15}
            tintColor={theme.textTertiary}
          />
        </Pressable>
      )}
    </View>
  );
}

const ACCESS_MODES: Array<{ id: RuntimeMode; description: string }> = [
  { id: 'ask', description: 'Approve every command and file edit.' },
  { id: 'autoAcceptEdits', description: 'Edits apply automatically; commands still ask.' },
  { id: 'auto', description: 'Works autonomously inside the project.' },
  { id: 'fullAccess', description: 'No approval prompts. The agent acts freely.' },
];

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
