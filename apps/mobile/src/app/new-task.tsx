import { useQuery } from '@tanstack/react-query';
import type { ProviderKind, RuntimeMode } from '@waku/client';
import {
  rememberedModelTraits,
  rememberComposerSession,
  type ComposerPreferences,
} from '@waku/client/composer-preferences';
import * as Haptics from 'expo-haptics';
import { router } from 'expo-router';
import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ActivityIndicator,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  View,
} from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { AppSymbol } from '@/components/app-symbol';
import { ComposerAccessMenu } from '@/components/composer-access-menu';
import { ComposerAddMenu } from '@/components/composer-add-menu';
import { ComposerContextPicker } from '@/components/composer-context-picker';
import { useComposerLocalCommands } from '@/components/composer-command-sheets';
import { DaemonPickerSheet } from '@/components/daemon-picker-sheet';
import {
  ComposerCard,
  ComposerIconButton,
  SendButton,
} from '@/components/mobile-composer';
import { RemoteProjectPicker } from '@/components/remote-project-picker';
import { useScreenHeaderInset } from '@/components/screen-header';
import {
  ModelPickerSheet,
  ModelTraitsSheet,
  type ProviderModelSelection,
} from '@/components/session-option-sheets';
import { Sheet, SheetRow } from '@/components/sheet';
import { Radius, Spacing } from '@/constants/theme';
import { useAllProviderModels, useProviderCatalog, useTaskState } from '@/hooks/use-daemon-data';
import { useComposerPicker } from '@/hooks/use-composer-picker';
import { useSyncedComposerDraft } from '@/hooks/use-synced-composer-draft';
import { useTheme } from '@/hooks/use-theme';
import { daemonKeys, inspectBranches } from '@/lib/daemon-api';
import { composerProviderPrompt } from '@/lib/composer-completion';
import {
  loadComposerPreferences,
  loadNewTaskExtras,
  saveComposerPreferences,
  saveNewTaskExtras,
} from '@/lib/composer-preferences-store';
import { useDaemon } from '@/lib/daemon-context';
import {
  modelHasConfigurableTraits,
  type ModelTraitSelection,
} from '@/lib/model-traits';
import { useRuntime } from '@/lib/runtime-context';
import { providerLabel } from '@/lib/session-presentation';

type SheetKind =
  | 'daemon'
  | 'project'
  | 'model'
  | 'traits'
  | 'workspace'
  | 'branch';

export default function NewTaskScreen() {
  const theme = useTheme();
  const insets = useSafeAreaInsets();
  const headerInset = useScreenHeaderInset();
  const daemon = useDaemon();
  const runtime = useRuntime();
  const taskState = useTaskState();
  const catalog = useProviderCatalog();
  const [projectId, setProjectId] = useState<string | null>(null);
  const [provider, setProvider] = useState<ProviderKind | null>(null);
  const [model, setModel] = useState<string | null>(null);
  const [reasoningEffort, setReasoningEffort] = useState<string | null>(null);
  const [serviceTier, setServiceTier] = useState<string | null>(null);
  const [contextWindow, setContextWindow] = useState<string | null>(null);
  const [runtimeMode, setRuntimeMode] = useState<RuntimeMode>('fullAccess');
  const [isolated, setIsolated] = useState(false);
  const [baseBranch, setBaseBranch] = useState<string | null>(null);
  const [prompt, setPrompt] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [openSheet, setOpenSheet] = useState<SheetKind | null>(null);
  const [projectPickerOpen, setProjectPickerOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const installedProviders = useMemo(
    () => catalog.providers.filter((item) => item.installed).map((item) => item.id),
    [catalog.providers],
  );
  const modelCatalog = useAllProviderModels(installedProviders);
  const projects = taskState.data?.projects ?? [];
  const selectedProject = projects.find((item) => item.id === projectId);
  const projectless = selectedProject?.name === 'No project';
  const branches = useQuery({
    queryKey: daemonKeys.branches(
      daemon.activeProfile?.id ?? 'disconnected',
      selectedProject?.path ?? 'missing',
    ),
    queryFn: () => inspectBranches(daemon.client!, selectedProject!.path),
    enabled: daemon.phase === 'connected' && Boolean(daemon.client && selectedProject) && isolated,
    staleTime: 30_000,
  });

  useEffect(() => {
    if (!projectId || !projects.some((project) => project.id === projectId)) {
      setProjectId(projects[0]?.id ?? null);
    }
  }, [projectId, projects]);

  useEffect(() => {
    if (projectless && isolated) setIsolated(false);
  }, [isolated, projectless]);

  useEffect(() => setBaseBranch(null), [projectId]);

  useEffect(() => {
    if (provider && installedProviders.includes(provider)) return;
    const preferred = installedProviders.includes('codex') ? 'codex' : installedProviders[0];
    if (preferred) {
      setProvider(preferred);
      setModel(null);
      setReasoningEffort(null);
      setServiceTier(null);
      setContextWindow(null);
    }
  }, [installedProviders, provider]);

  // Restore the last-used composition (provider/model traits via the shared
  // composer preferences, plus mobile extras) once per daemon.
  const [restoredAddress, setRestoredAddress] = useState<string | null>(null);
  const restoredFor = useRef<string | null>(null);
  const preferencesRef = useRef<ComposerPreferences | null>(null);
  useEffect(() => {
    const address = daemon.activeProfile?.address;
    if (!address || restoredFor.current === address) return;
    restoredFor.current = address;
    preferencesRef.current = null;
    setRestoredAddress(null);
    void (async () => {
      const [prefs, extras] = await Promise.all([
        loadComposerPreferences(address),
        loadNewTaskExtras(address),
      ]);
      setRuntimeMode(extras.runtimeMode);
      setIsolated(extras.isolated);
      if (extras.projectId) setProjectId(extras.projectId);
      setProvider(prefs.lastProvider);
      setModel(prefs.lastModel);
      setReasoningEffort(prefs.lastReasoningEffort);
      setServiceTier(prefs.lastServiceTier);
      setContextWindow(prefs.lastContextWindow);
      preferencesRef.current = prefs;
      setRestoredAddress(address);
    })();
  }, [daemon.activeProfile?.address]);

  // Unlike the web SPA, this screen unmounts between visits, so every choice
  // persists as it is made — not only when a task is created.
  useEffect(() => {
    const address = daemon.activeProfile?.address;
    if (!address || restoredAddress !== address) return;
    const timer = setTimeout(() => {
      void loadComposerPreferences(address).then((stored) => {
        const prefs = preferencesRef.current ?? stored;
        let next: ComposerPreferences = {
          ...prefs,
          ...(provider ? { lastProvider: provider } : {}),
          lastModel: model,
          lastReasoningEffort: reasoningEffort,
          lastServiceTier: serviceTier,
          lastContextWindow: contextWindow,
        };
        if (provider && model) {
          next = rememberComposerSession(next, {
            provider,
            model,
            reasoning_effort: reasoningEffort,
            service_tier: serviceTier,
            context_window: contextWindow,
          });
        }
        preferencesRef.current = next;
        return saveComposerPreferences(address, next);
      }).catch(() => {});
      void saveNewTaskExtras(address, {
        runtimeMode,
        isolated,
        projectId,
      }).catch(() => {});
    }, 300);
    return () => clearTimeout(timer);
  }, [
    contextWindow,
    daemon.activeProfile?.address,
    isolated,
    model,
    projectId,
    provider,
    reasoningEffort,
    restoredAddress,
    runtimeMode,
    serviceTier,
  ]);

  // Cross-device draft: hydrate again whenever this surface becomes active,
  // but persist only real local edits. Echoing a hydrated value would let a
  // backgrounded mobile client resurrect it after desktop submits it.
  const draftSync = useSyncedComposerDraft({
    target: selectedProject
      ? { type: 'newSession', projectId: selectedProject.id }
      : null,
    text: prompt,
    carryAcrossTargets: true,
    onHydrate: (synchronized) => setPrompt(synchronized.text),
  });
  const contextPicker = useComposerPicker({
    text: prompt,
    onChangeText: (value) => {
      draftSync.markEdited();
      setPrompt(value);
    },
    provider,
    root: selectedProject?.path ?? null,
    contextKey: selectedProject?.id ?? 'new-task',
  });

  function pick(apply: () => void) {
    return () => {
      void Haptics.selectionAsync();
      apply();
      setOpenSheet(null);
    };
  }

  function applyModelSelection(selection: ProviderModelSelection) {
    let preferences = preferencesRef.current;
    if (preferences && provider && model) {
      preferences = rememberComposerSession(preferences, {
        provider,
        model,
        reasoning_effort: reasoningEffort,
        service_tier: serviceTier,
        context_window: contextWindow,
      });
      preferencesRef.current = preferences;
    }
    const remembered = preferences && selection.model
      ? rememberedModelTraits(preferences, selection.provider, selection.model)
      : undefined;
    setProvider(selection.provider);
    setModel(selection.model);
    setReasoningEffort(remembered ? remembered.reasoningEffort : selection.reasoningEffort);
    setServiceTier(remembered ? remembered.serviceTier : selection.serviceTier);
    setContextWindow(remembered ? remembered.contextWindow : selection.contextWindow);
  }

  function applyModelTraits(changes: Partial<ModelTraitSelection>) {
    if (changes.reasoningEffort !== undefined) setReasoningEffort(changes.reasoningEffort);
    if (changes.serviceTier !== undefined) setServiceTier(changes.serviceTier);
    if (changes.contextWindow !== undefined) setContextWindow(changes.contextWindow);
  }

  async function start() {
    const value = prompt.trim();
    if (!selectedProject || !provider || !value || submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      const commands = value.startsWith('/') ? await contextPicker.getCommands() : [];
      if (await localCommands.execute(value, commands)) {
        setSubmitting(false);
        return;
      }
      const session = await runtime.createTask(
        selectedProject.id,
        provider,
        isolated && !projectless,
        value,
        {
          model,
          reasoningEffort,
          serviceTier,
          contextWindow,
          runtimeMode,
          baseBranch,
        },
        composerProviderPrompt(provider, value, commands),
      );
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
      const address = daemon.activeProfile?.address;
      if (address) {
        void loadComposerPreferences(address).then((stored) => {
          const prefs = preferencesRef.current ?? stored;
          let next = rememberComposerSession(prefs, session);
          if (!session.model) {
            next = {
              ...next,
              lastProvider: session.provider,
              lastModel: null,
              lastReasoningEffort: session.reasoning_effort ?? null,
              lastServiceTier: session.service_tier ?? null,
              lastContextWindow: session.context_window ?? null,
            };
          }
          preferencesRef.current = next;
          return saveComposerPreferences(address, next);
        }).catch(() => {});
        void saveNewTaskExtras(address, {
          runtimeMode,
          isolated: isolated && !projectless,
          projectId: selectedProject.id,
        }).catch(() => {});
      }
      draftSync.removeSubmittedDraft();
      setPrompt('');
      setSubmitting(false);
      router.push({ pathname: '/session/[id]', params: { id: session.id } });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
      setSubmitting(false);
    }
  }

  const providerModels = modelCatalog.find((entry) => entry.id === provider)?.models ?? [];
  const activeModel = model
    ? providerModels.find((item) => item.id === model)
    : providerModels.find((item) => item.is_default) ?? providerModels[0];
  const localCommands = useComposerLocalCommands({
    provider,
    model: activeModel,
    serviceTier,
    runtimeMode,
    contextKey: selectedProject?.id ?? 'new-task',
    onServiceTier: setServiceTier,
    onGoal: async (operation) => {
      if (!selectedProject || !provider) throw new Error('Choose a project and model first');
      const session = await runtime.createTask(selectedProject.id, provider, isolated && !projectless, '', {
        model, reasoningEffort, serviceTier, contextWindow, runtimeMode, baseBranch,
      });
      await runtime.sendGoalOperation(session, operation);
      router.push({ pathname: '/session/[id]', params: { id: session.id } });
    },
    onClear: () => {
      draftSync.markEdited();
      setPrompt('');
    },
  });
  const modelLabel = !provider
    ? catalog.isPending ? 'Checking agents…' : 'No agents installed'
    : activeModel?.name ?? model ?? providerLabel(provider);
  const branchLabel = baseBranch
    ?? branches.data?.default_branch
    ?? branches.data?.current
    ?? 'Default branch';
  const startDisabled = !selectedProject || !provider || !prompt.trim() || submitting;

  return (
    <KeyboardAvoidingView
      behavior={Platform.OS === 'ios' ? 'padding' : undefined}
      style={[styles.screen, { backgroundColor: theme.background }]}>
      {/* Title and back button are the native navigation bar's; keep clear of it. */}
      <View style={{ height: headerInset }} />
      <View style={styles.spacer} />

      <View style={styles.rows}>
        <SelectorRow
          icon={{ ios: 'laptopcomputer', android: 'laptop_mac', web: 'laptop_mac' }}
          label="Daemon"
          loading={
            daemon.phase === 'connecting'
            || daemon.phase === 'booting'
            || daemon.phase === 'reconnecting'
          }
          value={daemon.activeProfile?.name ?? 'Add a daemon'}
          onPress={() => setOpenSheet('daemon')}
        />
        <SelectorRow
          icon={{ ios: 'folder', android: 'folder', web: 'folder' }}
          label="Project"
          loading={taskState.isPending}
          value={selectedProject?.name ?? 'Choose a project'}
          onPress={() => setOpenSheet('project')}
        />
        <SelectorRow
          icon={{ ios: 'sparkle', android: 'auto_awesome', web: 'auto_awesome' }}
          label="Model"
          loading={catalog.isPending}
          value={modelLabel}
          onPress={() => setOpenSheet('model')}
        />
        <SelectorRow
          icon={{ ios: 'laptopcomputer', android: 'laptop_mac', web: 'laptop_mac' }}
          label="Workspace"
          value={isolated ? 'Isolated worktree' : 'Work locally'}
          onPress={() => setOpenSheet('workspace')}
        />
        {isolated && (
          <SelectorRow
            icon={{ ios: 'arrow.triangle.branch', android: 'account_tree', web: 'account_tree' }}
            label="Base branch"
            loading={branches.isPending && branches.fetchStatus !== 'idle'}
            value={branchLabel}
            onPress={() => setOpenSheet('branch')}
          />
        )}
      </View>

      <View style={[styles.composerShell, { paddingBottom: Math.max(insets.bottom, 10) }]}>
        {error && (
          <View
            accessibilityLiveRegion="polite"
            style={[styles.error, { backgroundColor: theme.dangerSoft }]}>
            <Text style={[styles.errorText, { color: theme.danger }]}>{error}</Text>
          </View>
        )}
        <ComposerCard
          {...contextPicker.inputProps}
          accessibilityLabel="Task prompt"
          autoFocus
          editable={!submitting}
          left={(
            <>
              <ComposerAddMenu
                disabled={submitting || daemon.phase !== 'connected' || !selectedProject || !provider}
                onChooseContext={contextPicker.open}
              />
              <ComposerAccessMenu
                mode={runtimeMode}
                onApply={setRuntimeMode}
              />
            </>
          )}
          placeholder={`Work on ${daemon.activeProfile?.name ?? 'your daemon'}`}
          right={(
            <>
              {activeModel && modelHasConfigurableTraits(activeModel) && (
                <ComposerIconButton
                  icon={{ ios: 'speedometer', android: 'speed', web: 'speed' }}
                  label="Model options"
                  onPress={() => setOpenSheet('traits')}
                />
              )}
              <SendButton
                busy={submitting}
                disabled={startDisabled}
                label="Start task"
                onPress={() => void start()}
              />
            </>
          )}
          value={prompt}
        />
      </View>

      <DaemonPickerSheet
        onDismiss={() => setOpenSheet(null)}
        visible={openSheet === 'daemon'}
      />

      <Sheet onDismiss={() => setOpenSheet(null)} title="Project" visible={openSheet === 'project'}>
        {projects.map((project) => (
          <SheetRow
            description={project.path}
            key={project.id}
            label={project.name}
            onPress={pick(() => setProjectId(project.id))}
            selected={project.id === projectId}
          />
        ))}
        <SheetRow
          description="Pick any folder on the daemon host, or create an empty workspace"
          label="Browse daemon host…"
          leading={(
            <AppSymbol
              name={{ ios: 'externaldrive', android: 'hard_drive', web: 'hard_drive' }}
              size={16}
              tintColor={theme.textSecondary}
            />
          )}
          onPress={() => {
            setOpenSheet(null);
            setProjectPickerOpen(true);
          }}
        />
      </Sheet>

      <ModelPickerSheet
        model={model}
        onApply={applyModelSelection}
        onDismiss={() => setOpenSheet(null)}
        provider={provider}
        providers={installedProviders}
        visible={openSheet === 'model'}
      />

      {activeModel && (
        <ModelTraitsSheet
          model={activeModel}
          onApply={applyModelTraits}
          onDismiss={() => setOpenSheet(null)}
          selection={{ reasoningEffort, serviceTier, contextWindow }}
          visible={openSheet === 'traits'}
        />
      )}

      <Sheet onDismiss={() => setOpenSheet(null)} title="Workspace" visible={openSheet === 'workspace'}>
        <SheetRow
          description="Run in the project checkout"
          label="Work locally"
          onPress={pick(() => setIsolated(false))}
          selected={!isolated}
        />
        <SheetRow
          description="A separate branch and folder that never touches the checkout"
          disabled={projectless}
          label="Isolated worktree"
          onPress={pick(() => setIsolated(true))}
          selected={isolated}
        />
      </Sheet>

      <Sheet onDismiss={() => setOpenSheet(null)} title="Base branch" visible={openSheet === 'branch'}>
        {branches.isPending ? (
          <View style={styles.sheetLoading}>
            <ActivityIndicator color={theme.textTertiary} />
          </View>
        ) : branches.error ? (
          <Text style={[styles.sheetNote, { color: theme.danger }]}>
            {branches.error instanceof Error ? branches.error.message : String(branches.error)}
          </Text>
        ) : !branches.data ? (
          <Text style={[styles.sheetNote, { color: theme.textTertiary }]}>
            This project isn’t a Git repository.
          </Text>
        ) : (
          branches.data.branches.map((branch) => (
            <SheetRow
              description={branch.name === branches.data?.current ? 'Current branch' : undefined}
              key={branch.name}
              label={branch.name}
              onPress={pick(() => setBaseBranch(branch.name))}
              selected={branch.name === (baseBranch ?? branches.data?.default_branch ?? branches.data?.current)}
            />
          ))
        )}
      </Sheet>

      <RemoteProjectPicker
        visible={projectPickerOpen}
        onDismiss={() => setProjectPickerOpen(false)}
        onSelect={(project) => setProjectId(project.id)}
      />
      {localCommands.sheets}
      <ComposerContextPicker {...contextPicker.picker} />
    </KeyboardAvoidingView>
  );
}

function SelectorRow({
  icon,
  label,
  value,
  onPress,
  loading = false,
}: {
  icon: Parameters<typeof AppSymbol>[0]['name'];
  label: string;
  value: string;
  onPress: () => void;
  loading?: boolean;
}) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityLabel={`${label}: ${value}`}
      accessibilityRole="button"
      disabled={loading}
      onPress={onPress}
      style={({ pressed }) => [
        styles.row,
        { backgroundColor: pressed ? theme.overlay : 'transparent' },
      ]}>
      <AppSymbol name={icon} size={19} tintColor={theme.textSecondary} />
      {loading ? (
        <ActivityIndicator color={theme.textTertiary} size="small" />
      ) : (
        <Text numberOfLines={1} style={[styles.rowValue, { color: theme.text }]}>
          {value}
        </Text>
      )}
      <AppSymbol
        name={{ ios: 'chevron.up.chevron.down', android: 'unfold_more', web: 'unfold_more' }}
        size={13}
        tintColor={theme.textTertiary}
      />
    </Pressable>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  spacer: { flex: 1 },
  rows: { gap: 2, paddingBottom: 8, paddingHorizontal: Spacing.three },
  row: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    flexDirection: 'row',
    gap: 14,
    minHeight: 52,
    paddingHorizontal: 6,
  },
  rowValue: { flexShrink: 1, fontSize: 16.5, fontWeight: '500' },
  composerShell: { paddingHorizontal: 12 },
  error: { borderRadius: Radius.medium, marginBottom: 8, padding: 11 },
  errorText: { fontSize: 12.5, fontWeight: '600', lineHeight: 17 },
  sheetLoading: { alignItems: 'center', paddingVertical: 14 },
  sheetSection: {
    fontSize: 11,
    fontWeight: '700',
    letterSpacing: 0.5,
    marginBottom: 4,
    marginHorizontal: 12,
    marginTop: 12,
  },
  sheetNote: { fontSize: 13, lineHeight: 18, paddingHorizontal: 12, paddingVertical: 10 },
});
