import { useQuery, useQueryClient } from '@tanstack/react-query';
import type {
  GoalOperation,
  ProviderKind,
  ProviderModel,
  ProviderSessionSummary,
  RuntimeMode,
  SlashCommand,
  ThreadGoal,
} from '@waku/client';
import {
  isFastModeToggleSubmission,
  isResumeSubmission,
  parseGoalSubmission,
  toggledFastServiceTier,
} from '@waku/client/composer-autocomplete';
import { fuzzyScore } from '@waku/client/fuzzy-search';
import * as Crypto from 'expo-crypto';
import { router } from 'expo-router';
import { useEffect, useMemo, useState } from 'react';
import { ActivityIndicator, FlatList, StyleSheet, Text, TextInput, View, useWindowDimensions } from 'react-native';

import { Sheet, SheetRow } from './sheet';
import { NativeTint, Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import {
  createProject,
  daemonKeys,
  listProviderSessions,
  loadProviderSessionHistory,
  loadTaskState,
  persistProject,
  persistSession,
  providerSessionKey,
} from '@/lib/daemon-api';
import { useDaemon } from '@/lib/daemon-context';
import { createResumedSession } from '@/lib/mobile-runtime';
import { providerLabel } from '@/lib/session-presentation';

/** Desktop's local commands must never leak through as provider prompts. */
export function useComposerLocalCommands({
  provider,
  model,
  serviceTier,
  runtimeMode,
  goal,
  contextKey,
  onServiceTier,
  onGoal,
  onClear,
}: {
  provider: ProviderKind | null;
  model: ProviderModel | undefined;
  serviceTier: string | null | undefined;
  runtimeMode: RuntimeMode;
  goal?: ThreadGoal | null;
  contextKey: string;
  onServiceTier: (tier: string) => void | Promise<void>;
  onGoal: (operation: GoalOperation) => Promise<void>;
  onClear: () => void;
}) {
  const daemon = useDaemon();
  const [resumeOpen, setResumeOpen] = useState(false);
  const [goalDialog, setGoalDialog] = useState<{ prefill: string | null; replace: boolean } | null>(null);
  useEffect(() => {
    setResumeOpen(false);
    setGoalDialog(null);
  }, [contextKey, daemon.activeProfile?.id, provider]);

  async function execute(prompt: string, commands: SlashCommand[]): Promise<boolean> {
    if (!provider) return false;
    if (isResumeSubmission(prompt)) {
      setResumeOpen(true);
    } else if (isFastModeToggleSubmission(provider, prompt, commands)) {
      const tier = toggledFastServiceTier(serviceTier, model?.service_tiers ?? []);
      if (!tier) throw new Error('Fast mode is unavailable for this model');
      await onServiceTier(tier);
    } else {
      const command = parseGoalSubmission(provider, prompt, commands);
      if (!command) return false;
      switch (command.kind) {
        case 'show':
        case 'edit':
          setGoalDialog({ prefill: null, replace: false });
          break;
        case 'pause':
        case 'resume':
          await onGoal({ kind: 'set', objective: null, status: command.kind === 'pause' ? 'paused' : 'active', replace: false });
          break;
        case 'clear':
          await onGoal({ kind: 'clear' });
          break;
        case 'set':
          if (goal && goal.status !== 'complete' && goal.status !== 'budgetLimited') {
            setGoalDialog({ prefill: command.objective, replace: true });
          } else {
            await onGoal({ kind: 'set', objective: command.objective, status: 'active', replace: Boolean(goal) });
          }
          break;
      }
    }
    onClear();
    return true;
  }

  return {
    execute,
    sheets: (
      <>
        {resumeOpen && provider && (
          <ResumeSessionSheet provider={provider} runtimeMode={runtimeMode} onDismiss={() => setResumeOpen(false)} />
        )}
        {goalDialog && (
          <GoalCommandSheet
            goal={goal ?? null}
            prefill={goalDialog.prefill}
            replace={goalDialog.replace}
            onRun={onGoal}
            onDismiss={() => setGoalDialog(null)}
          />
        )}
      </>
    ),
  };
}

function ResumeSessionSheet({ provider, runtimeMode, onDismiss }: {
  provider: ProviderKind;
  runtimeMode: RuntimeMode;
  onDismiss: () => void;
}) {
  const theme = useTheme();
  const daemon = useDaemon();
  const queryClient = useQueryClient();
  const { height } = useWindowDimensions();
  const profileId = daemon.activeProfile?.id ?? 'disconnected';
  const [search, setSearch] = useState('');
  const [resuming, setResuming] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const sessions = useQuery({
    queryKey: daemonKeys.providerSessions(profileId, provider),
    queryFn: () => listProviderSessions(daemon.client!, provider),
    enabled: daemon.phase === 'connected' && Boolean(daemon.client),
    staleTime: 15_000,
  });
  const rows = useMemo(() => (sessions.data ?? []).filter((item) =>
    fuzzyScore(search, `${item.title} ${item.cwd}`) !== null), [search, sessions.data]);

  async function resume(summary: ProviderSessionSummary) {
    const client = daemon.client;
    if (!client || daemon.phase !== 'connected' || resuming) return;
    setResuming(providerSessionKey(summary.cursor));
    setError(null);
    try {
      const state = await loadTaskState(client);
      let session = state.sessions.find((item) => item.provider_cursor
        && providerSessionKey(item.provider_cursor) === providerSessionKey(summary.cursor));
      if (!session) {
        const history = await loadProviderSessionHistory(client, summary);
        let project = state.projects.find((item) => item.path === summary.cwd);
        if (!project) project = (await persistProject(client, createProject(summary.cwd, Crypto.randomUUID()))).project;
        session = await persistSession(client, createResumedSession(
          project.id, summary, history, runtimeMode,
          { nowSeconds: () => Math.floor(Date.now() / 1_000), randomUUID: Crypto.randomUUID },
        ));
        queryClient.setQueryData(daemonKeys.session(profileId, session.id), session);
      }
      await queryClient.invalidateQueries({ queryKey: daemonKeys.taskState(profileId) });
      onDismiss();
      router.push({ pathname: '/session/[id]', params: { id: session.id } });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setResuming(null);
    }
  }

  return (
    <Sheet visible onDismiss={onDismiss} scrollable={false} title={`Resume ${providerLabel(provider)} session`}>
      <TextInput
        accessibilityLabel="Search provider sessions"
        placeholder="Search sessions…"
        placeholderTextColor={theme.textTertiary}
        selectionColor={NativeTint}
        style={[styles.input, { color: theme.text, backgroundColor: theme.inset }]}
        value={search}
        onChangeText={setSearch}
      />
      {(error || sessions.error) && <Text style={[styles.note, { color: theme.danger }]}>{error ?? String(sessions.error)}</Text>}
      {sessions.isPending ? <ActivityIndicator style={styles.loading} color={theme.textTertiary} /> : (
        <FlatList
          data={rows}
          extraData={resuming}
          initialNumToRender={8}
          keyExtractor={(item) => providerSessionKey(item.cursor)}
          keyboardShouldPersistTaps="handled"
          style={{ maxHeight: height * 0.45 }}
          ListEmptyComponent={<Text style={[styles.note, { color: theme.textTertiary }]}>No matching sessions</Text>}
          renderItem={({ item }) => (
            <SheetRow
              label={item.title || 'Untitled session'}
              description={item.cwd}
              disabled={Boolean(resuming)}
              leading={resuming === providerSessionKey(item.cursor) ? <ActivityIndicator size="small" /> : undefined}
              onPress={() => void resume(item)}
            />
          )}
        />
      )}
      {sessions.error && <SheetRow label="Retry" onPress={() => { void sessions.refetch(); }} />}
    </Sheet>
  );
}

function GoalCommandSheet({ goal, prefill, replace, onRun, onDismiss }: {
  goal: ThreadGoal | null;
  prefill: string | null;
  replace: boolean;
  onRun: (operation: GoalOperation) => Promise<void>;
  onDismiss: () => void;
}) {
  const theme = useTheme();
  const [objective, setObjective] = useState(prefill ?? goal?.objective ?? '');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function run(operation: GoalOperation) {
    if (pending) return;
    setPending(true);
    setError(null);
    try {
      await onRun(operation);
      onDismiss();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setPending(false);
    }
  }
  return (
    <Sheet visible onDismiss={onDismiss} title="Goal">
      {goal && <Text style={[styles.note, { color: theme.textSecondary }]}>Status: {{
        active: 'Active', paused: 'Paused', blocked: 'Stalled', usageLimited: 'Usage limit reached',
        budgetLimited: 'Budget reached', complete: 'Complete',
      }[goal.status]}</Text>}
      <TextInput
        accessibilityLabel="Goal objective"
        multiline
        editable={!pending}
        placeholder="What should the agent keep working on?"
        placeholderTextColor={theme.textTertiary}
        selectionColor={NativeTint}
        style={[styles.input, styles.objective, { color: theme.text, backgroundColor: theme.inset }]}
        value={objective}
        onChangeText={setObjective}
      />
      {replace && goal && <Text style={[styles.note, { color: theme.textSecondary }]}>This replaces the current goal: {goal.objective}</Text>}
      {error && <Text style={[styles.note, { color: theme.danger }]}>{error}</Text>}
      <SheetRow
        label={goal ? replace ? 'Replace goal' : 'Save goal' : 'Set goal'}
        disabled={pending || !objective.trim()}
        leading={pending ? <ActivityIndicator size="small" /> : undefined}
        onPress={() => void run({
          kind: 'set', objective: objective.trim(),
          status: goal && !replace && goal.status !== 'complete' && goal.status !== 'budgetLimited'
            ? goal.status : 'active',
          replace: replace && Boolean(goal),
        })}
      />
      {goal && !replace && (
        <>
          {(goal.status === 'active' || goal.status === 'paused' || goal.status === 'blocked' || goal.status === 'usageLimited') && <SheetRow
            label={goal.status === 'active' ? 'Pause goal' : 'Resume goal'}
            disabled={pending}
            onPress={() => void run({ kind: 'set', objective: null, status: goal.status === 'active' ? 'paused' : 'active', replace: false })}
          />}
          <SheetRow label="Clear goal" destructive disabled={pending} onPress={() => void run({ kind: 'clear' })} />
        </>
      )}
    </Sheet>
  );
}

const styles = StyleSheet.create({
  input: { borderRadius: Radius.medium, fontSize: 15, minHeight: 44, paddingHorizontal: 12, paddingVertical: 10, marginHorizontal: 6, marginBottom: 8 },
  objective: { minHeight: 88, maxHeight: 180, textAlignVertical: 'top' },
  note: { fontSize: 13, lineHeight: 18, paddingHorizontal: 12, paddingVertical: 8 },
  loading: { paddingVertical: 20 },
});
