import * as Clipboard from 'expo-clipboard';
import * as Haptics from 'expo-haptics';
import { MenuView, type MenuAction } from '@expo/ui/community/menu';

import {
  router,
  Stack,
  type NativeStackHeaderItem,
  type NativeStackNavigationOptions,
} from 'expo-router';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Alert,
  KeyboardAvoidingView,
  Platform,
  StyleSheet,
  Text,
  View,
} from 'react-native';

import { ActivitySheetHost } from '@/components/activity-sheet';
import { ConnectionBanner } from '@/components/connection-banner';
import { MobileComposer } from '@/components/mobile-composer';
import { RenameDialog } from '@/components/rename-dialog';
import {
  HeaderAction,
  HeaderActionGroup,
  HeaderMenuTrigger,
  HeaderTitle,
  ScreenHeaderBackdrop,
  nativeHeaderButtons,
  navigateBack,
  useScreenHeaderInset,
  type HeaderActionSpec,
} from '@/components/screen-header';
import { TurnSheet } from '@/components/session-option-sheets';
import {
  TaskSurfaceSheet,
  type TaskSurface,
} from '@/components/task-surface-sheet';
import { useTaskDrawer } from '@/components/task-drawer';
import {
  TranscriptList,
  type TranscriptDevSample,
  type TranscriptListHandle,
} from '@/components/transcript-list';
import { SessionEmpty } from '@/components/transcript-rows';
import { useSession, useTaskState } from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import { useDaemon } from '@/lib/daemon-context';
import { sessionBusy } from '@/lib/mobile-runtime';
import { useRuntime } from '@/lib/runtime-context';
import {
  displaySessionTitle,
  turnOptionsForSession,
} from '@/lib/session-presentation';

const SURFACE_MENU_COMMANDS = [
  { id: 'terminal', title: 'Terminal', symbol: 'terminal' },
  { id: 'files', title: 'Files', symbol: 'folder' },
  { id: 'review', title: 'Review', symbol: 'doc.text.magnifyingglass' },
] as const;

const TASK_MENU_COMMANDS = [
  { id: 'rename', title: 'Rename task', symbol: 'pencil', destructive: false },
  {
    id: 'copy-last-response',
    title: 'Copy last response',
    symbol: 'doc.on.doc',
    destructive: false,
  },
  {
    id: 'rewind',
    title: 'Rewind to turn…',
    symbol: 'clock.arrow.circlepath',
    destructive: true,
  },
  {
    id: 'fork',
    title: 'Fork from turn…',
    symbol: 'arrow.branch',
    destructive: false,
  },
  {
    id: 'reload',
    title: 'Reload transcript',
    symbol: 'arrow.clockwise',
    destructive: false,
  },
  { id: 'delete', title: 'Delete task', symbol: 'trash', destructive: true },
] as const;



export function SessionView({
  sessionId,
  devPrompt,
}: {
  sessionId: string | undefined;
  /** Dev-only: auto-submit this prompt through the composer path once the
   * session loads — lets headless rigs exercise the exact user flow. */
  devPrompt?: string;
}) {
  const theme = useTheme();
  const daemon = useDaemon();
  const runtime = useRuntime();
  const { openTaskDrawer } = useTaskDrawer();
  const query = useSession(sessionId);
  const session = query.data;
  const [taskSurface, setTaskSurface] = useState<TaskSurface | null>(null);
  const [taskSurfaceOpen, setTaskSurfaceOpen] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [turnTarget, setTurnTarget] = useState<'rewind' | 'fork' | null>(null);
  const [underHeader, setUnderHeader] = useState(false);
  const [mountedTranscriptSessionId, setMountedTranscriptSessionId] = useState<string | null>(null);
  const running = Boolean(session && sessionBusy(session));
  const listRef = useRef<TranscriptListHandle>(null);
  const headerInset = useScreenHeaderInset();
  const sessionRef = useRef(session);
  const queryRef = useRef(query);
  const runtimeRef = useRef(runtime);
  sessionRef.current = session;
  queryRef.current = query;
  runtimeRef.current = runtime;

  useEffect(() => {
    if (!session || daemon.phase !== 'connected') return;
    void runtime.attachSession(session).catch(() => {});
    // Re-runs when the session starts working (another client may have
    // started the runtime after this screen mounted).
  }, [daemon.phase, runtime.attachSession, session?.id, running]);

  // Transient task chrome belongs to one session. A route reuse must not show
  // the previous task's file, review, or terminal surface.
  useEffect(() => {
    setUnderHeader(false);
    setTaskSurfaceOpen(false);
    setTaskSurface(null);
  }, [session?.id]);

  // Route/header/composer get the first commit by themselves. Transcript row
  // construction includes pipeline building and Markdown expansion, so doing
  // it in the route's first render delays the entire native screen appearing.
  // Two frames guarantee the lightweight task shell has painted before that
  // synchronous presentation work begins.
  useEffect(() => {
    const sessionId = session?.id;
    if (!sessionId) {
      setMountedTranscriptSessionId(null);
      return;
    }
    let mountFrame = 0;
    const shellFrame = requestAnimationFrame(() => {
      mountFrame = requestAnimationFrame(() => setMountedTranscriptSessionId(sessionId));
    });
    return () => {
      cancelAnimationFrame(shellFrame);
      if (mountFrame) cancelAnimationFrame(mountFrame);
    };
  }, [session?.id]);

  const probe = useDevProbe(Boolean(devPrompt));

  // Dev-only auto-submit: same path as the composer's send button.
  const devPromptSent = useRef(false);
  useEffect(() => {
    if (!devPrompt || devPromptSent.current) return;
    if (!session || query.isPlaceholderData) {
      probe.setStatus('waiting for session');
      return;
    }
    if (daemon.phase !== 'connected') {
      probe.setStatus(`daemon ${daemon.phase}`);
      return;
    }
    if (sessionBusy(session)) {
      probe.setStatus('session busy');
      return;
    }
    devPromptSent.current = true;
    probe.setStatus('submitting');
    listRef.current?.followNextGrowth();
    runtime.sendPrompt(session, devPrompt)
      .then(() => probe.setStatus('submitted'))
      .catch((cause) => probe.setStatus(`failed ${String(cause).slice(0, 120)}`));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [daemon.phase, devPrompt, query.isPlaceholderData, session]);

  const openTaskSurface = useCallback((surface: TaskSurface) => {
    setTaskSurface(surface);
    setTaskSurfaceOpen(true);
  }, []);

  const copyLastResponse = useCallback(async () => {
    const lastAssistant = [...(sessionRef.current?.messages ?? [])]
      .reverse()
      .find((message) => message.role === 'assistant' && message.content.trim());
    if (!lastAssistant) return;
    await Clipboard.setStringAsync(lastAssistant.content);
    await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
  }, []);

  const confirmDelete = useCallback(() => {
    const current = sessionRef.current;
    if (!current) return;
    Alert.alert(
      `Delete “${displaySessionTitle(current)}”?`,
      'This removes the task and its transcript from the daemon for every device.',
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Delete',
          style: 'destructive',
          onPress: () => {
            void runtimeRef.current.deleteSession(current.id)
              .then(() => {
                void Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
                navigateBack();
              })
              .catch((cause) => {
                Alert.alert('Couldn’t delete task', cause instanceof Error ? cause.message : String(cause));
              });
          },
        },
      ],
    );
  }, []);

  const turnOptions = useMemo(() => turnOptionsForSession(session), [session]);

  // Rewinding or forking rewrites history the daemon is still appending to, so
  // both are refused while a turn is in flight rather than racing it.
  const openTurnTarget = useCallback((mode: 'rewind' | 'fork') => {
    const current = sessionRef.current;
    if (!current) return;
    if (sessionBusy(current)) {
      Alert.alert(
        'Task is busy',
        'Let the current turn finish before changing this task’s history.',
      );
      return;
    }
    if (!turnOptionsForSession(current).length) {
      Alert.alert('Nothing to rewind', 'This task has no completed turn yet.');
      return;
    }
    setTurnTarget(mode);
  }, []);

  // The sheet only picks a target; the daemon call waits for this confirmation.
  const applyTurnTarget = useCallback((turnCount: number) => {
    const mode = turnTarget;
    const current = sessionRef.current;
    if (!mode || !current) return;
    const rewind = mode === 'rewind';
    Alert.alert(
      rewind ? `Rewind to turn ${turnCount}?` : `Fork at turn ${turnCount}?`,
      rewind
        ? 'Every turn after it is removed for every device, and the workspace is restored to that point.'
        : 'A copy of the task up to that turn is created. This task keeps its own history.',
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: rewind ? 'Rewind' : 'Fork',
          style: rewind ? 'destructive' : 'default',
          onPress: () => {
            const action = rewind
              ? runtimeRef.current.rewindSession(current.id, turnCount)
              : runtimeRef.current.forkSession(current.id, turnCount);
            void action
              .then(async (result) => {
                await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
                if (rewind) {
                  if (result.warning) Alert.alert('Rewound', result.warning);
                  return;
                }
                Alert.alert('Task forked', result.warning ?? 'It is in your task history.', [
                  { text: 'Later' },
                  {
                    text: 'Open',
                    onPress: () => router.replace({
                      pathname: '/session/[id]',
                      params: { id: result.session.id },
                    }),
                  },
                ]);
              })
              .catch((cause) => {
                Alert.alert(
                  rewind ? 'Couldn’t rewind task' : 'Couldn’t fork task',
                  cause instanceof Error ? cause.message : String(cause),
                );
              });
          },
        },
      ],
    );
  }, [turnTarget]);

  const handleTaskMenuCommand = useCallback(
    (command: string) => {
      if (command === 'terminal' || command === 'files' || command === 'review') {
        openTaskSurface(command);
      } else if (command === 'rewind' || command === 'fork') {
        openTurnTarget(command);
      } else if (command === 'rename') {
        setRenaming(true);
      } else if (command === 'copy-last-response') {
        void copyLastResponse();
      } else if (command === 'reload') {
        void queryRef.current.refetch();
      } else if (command === 'delete') {
        confirmDelete();
      }
    },
    [confirmDelete, copyLastResponse, openTaskSurface, openTurnTarget],
  );

  const taskState = useTaskState().data;
  const project = taskState?.projects.find((item) => item.id === session?.project_id);
  const subtitleParts = [project?.name, daemon.activeProfile?.name].filter(Boolean);
  const title = session ? displaySessionTitle(session) : 'Task';
  // The bar's subtitle doubles as the quiet link indicator, the way messaging
  // apps do it; the banner below only steps in when the outage persists.
  const linkSubtitle = daemon.phase === 'reconnecting'
    ? daemon.outage?.interrupted ? 'Reconnecting…' : 'Connecting…'
    : daemon.phase === 'connecting' || daemon.phase === 'booting'
      ? 'Connecting…'
      : daemon.phase === 'error' ? 'Not connected' : null;
  const subtitle = linkSubtitle ?? (subtitleParts.length ? subtitleParts.join(' · ') : null);
  const hasSession = Boolean(session);
  const transcriptMounted = Boolean(
    session && mountedTranscriptSessionId === session.id,
  );
  const taskMenuActions = useMemo<MenuAction[]>(
    () => [
      ...SURFACE_MENU_COMMANDS.map((item) => ({
        id: item.id,
        title: item.title,
        image: item.symbol,
      })),
      {
        id: 'task-actions',
        title: '',
        displayInline: true,
        subactions: TASK_MENU_COMMANDS.map((item) => ({
          id: item.id,
          title: item.title,
          image: item.symbol,
          attributes: item.destructive ? { destructive: true } : undefined,
        })),
      },
    ],
    [],
  );

  // The chrome lives in the native navigation bar, so it stays put while the
  // page slides under a swipe-back. Keyed on the strings, not the session, so
  // streaming updates never touch the bar.
  const headerOptions = useMemo<NativeStackNavigationOptions>(() => {
    const drawer: HeaderActionSpec = {
      icon: { ios: 'sidebar.left', android: 'menu', web: 'menu' },
      label: 'Task history',
      onPress: openTaskDrawer,
    };
    const newTask: HeaderActionSpec = {
      icon: { ios: 'square.and.pencil', android: 'edit_square', web: 'edit' },
      label: 'New task',
      onPress: () => router.dismissTo('/'),
    };
    const nativeItems: NativeStackHeaderItem[] = hasSession
      ? [
          ...nativeHeaderButtons([newTask]),
          {
            type: 'menu',
            label: 'Task options',
            accessibilityLabel: 'Task options',
            icon: { type: 'sfSymbol', name: 'ellipsis' },
            menu: {
              title,
              // These are commands, not a single-selection picker.
              multiselectable: true,
              items: [
                ...SURFACE_MENU_COMMANDS.map((item) => ({
                  type: 'action' as const,
                  label: item.title,
                  icon: { type: 'sfSymbol' as const, name: item.symbol },
                  onPress: () => handleTaskMenuCommand(item.id),
                })),
                {
                  type: 'submenu',
                  label: '',
                  inline: true,
                  multiselectable: true,
                  items: TASK_MENU_COMMANDS.map((item) => ({
                    type: 'action' as const,
                    label: item.title,
                    icon: { type: 'sfSymbol' as const, name: item.symbol },
                    destructive: item.destructive,
                    onPress: () => handleTaskMenuCommand(item.id),
                  })),
                },
              ],
            },
          },
        ]
      : [];
    return {
      headerTitle: Platform.OS === 'ios'
        ? ''
        : () => <HeaderTitle subtitle={subtitle} title={title} />,
      headerTitleAlign: 'left',
      headerRight: hasSession
        ? () => (
            <HeaderActionGroup>
              <HeaderAction {...newTask} />
              <MenuView
                actions={taskMenuActions}
                title={title}
                onPressAction={({ nativeEvent }) =>
                  handleTaskMenuCommand(nativeEvent.event)
                }
              >
                <HeaderMenuTrigger
                  icon={{
                    ios: 'ellipsis',
                    android: 'more_horiz',
                    web: 'more_horiz',
                  }}
                  label="Task options"
                />
              </MenuView>
            </HeaderActionGroup>
          )
        : undefined,
      unstable_headerRightItems: nativeItems.length
        ? () => nativeItems
        : undefined,
      unstable_headerLeftItems: Platform.OS === 'ios'
        ? () => [
            ...nativeHeaderButtons([drawer]),
            {
              type: 'custom' as const,
              element: <HeaderTitle subtitle={subtitle} title={title} />,
              hidesSharedBackground: true,
            },
          ]
        : undefined,
    };
  }, [handleTaskMenuCommand, hasSession, openTaskDrawer, subtitle, taskMenuActions, title]);

  return (
    <KeyboardAvoidingView
      behavior={Platform.OS === 'ios' ? 'padding' : undefined}
      style={[styles.screen, { backgroundColor: theme.background }]}>
      <Stack.Screen options={headerOptions} />
      <View style={styles.body}>
        {session && transcriptMounted ? (
          <ActivitySheetHost key={`activity-sheet:${session.id}`} session={session}>
            <TranscriptList
              headerInset={headerInset}
              hydrated={!query.isPlaceholderData}
              ref={listRef}
              running={running}
              session={session}
              onDevSample={devPrompt ? probe.sample : undefined}
              onUnderHeaderChange={setUnderHeader}
            />
          </ActivitySheetHost>
        ) : (
          <View style={styles.placeholder}>
            <SessionEmpty
              error={query.error}
              loading={Boolean(session) || query.isPending}
              missing={query.data === null}
            />
          </View>
        )}
        <View pointerEvents="box-none" style={[styles.linkBanner, { top: headerInset + 8 }]}>
          <ConnectionBanner floating />
        </View>
        {Boolean(devPrompt && probe.text) && (
          <View pointerEvents="none" style={[styles.devBadge, { top: headerInset + 8 }]}>
            <Text style={styles.devBadgeText}>{probe.text}</Text>
          </View>
        )}
      </View>
      <ScreenHeaderBackdrop visible={underHeader} />
      {session && (
        <MobileComposer
          key={`composer:${session.id}`}
          session={session}
          onSubmitted={() => listRef.current?.followNextGrowth()}
        />
      )}

      {session && (
        <TaskSurfaceSheet
          key={`task-surface:${session.id}`}
          onDismiss={() => {
            setTaskSurfaceOpen(false);
          }}
          project={project}
          session={session}
          surface={taskSurface}
          visible={taskSurfaceOpen}
        />
      )}
      {session && (
        <RenameDialog
          initialValue={displaySessionTitle(session)}
          onDismiss={() => setRenaming(false)}
          onSubmit={(title) => runtime.renameSession(session.id, title)}
          visible={renaming}
        />
      )}
      <TurnSheet
        note={turnTarget === 'rewind'
          ? 'Everything after the turn you pick is removed for every device.'
          : 'The copy keeps the turns up to the one you pick.'}
        onDismiss={() => setTurnTarget(null)}
        onPick={applyTurnTarget}
        title={turnTarget === 'rewind' ? 'Rewind to turn' : 'Fork from turn'}
        turns={turnOptions}
        visible={Boolean(turnTarget)}
      />
    </KeyboardAvoidingView>
  );
}

/**
 * Dev-only motion probe. Screen recorders capture ~8 real fps and cannot
 * tell a seated stream from a bouncing one; the scroll events can. While a
 * stream is followed the native offset must read 0 — `drift` is the largest
 * untouched offset seen in the last second, and `grow` counts content-size
 * commits, so "drift 0" across a fast stream is the pass condition.
 */
function useDevProbe(enabled: boolean) {
  const [status, setStatus] = useState('');
  const [text, setText] = useState('');
  const samples = useRef<TranscriptDevSample[]>([]);
  const rates = useRef<Array<{ at: number; count: number; flips: number }>>([]);
  const growths = useRef(0);
  const lastHeight = useRef(0);

  const sample = useCallback((next: TranscriptDevSample) => {
    if (next.contentHeight !== lastHeight.current) {
      if (lastHeight.current > 0) growths.current += 1;
      lastHeight.current = next.contentHeight;
    }
    samples.current.push(next);
  }, []);

  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(() => {
      const now = Date.now();
      const recent = samples.current.filter((item) => now - item.at < 1_000);
      samples.current = recent;
      let drift = 0;
      for (const item of recent) {
        if (!item.touching) drift = Math.max(drift, item.offset);
      }
      const latest = recent.at(-1);
      const scrolls = recent.filter((item) => item.source === 'scroll');
      // Direction reversals between consecutive scroll samples: an animation
      // is monotonic, a compensation fight alternates.
      let flips = 0;
      for (let ix = 2; ix < scrolls.length; ix += 1) {
        const a = scrolls[ix - 1]!.offset - scrolls[ix - 2]!.offset;
        const b = scrolls[ix]!.offset - scrolls[ix - 1]!.offset;
        if (a * b < 0) flips += 1;
      }
      // Peaks over the last five seconds survive the snapshot latency of an
      // external reader.
      rates.current = [
        ...rates.current.filter((item) => now - item.at < 5_000),
        { at: now, count: scrolls.length, flips },
      ];
      const peak = Math.max(...rates.current.map((item) => item.count));
      const peakFlips = Math.max(...rates.current.map((item) => item.flips));
      setText(
        `dev: ${status} · scr ${scrolls.length}/s (peak ${peak}, flips ${peakFlips}) · size ${recent.length - scrolls.length}/s · off ${latest ? latest.offset.toFixed(0) : '–'} · drift ${drift.toFixed(0)} · grow ${growths.current} · touch ${latest ? (latest.touching ? 1 : 0) : '–'}`,
      );
    }, 500);
    return () => clearInterval(timer);
  }, [enabled, status]);

  return { sample, setStatus, text: enabled ? text : '' };
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  body: { flex: 1 },
  placeholder: { alignItems: 'center', flex: 1, justifyContent: 'center', paddingHorizontal: 32 },
  linkBanner: { left: 12, position: 'absolute', right: 12, zIndex: 10 },
  devBadge: {
    backgroundColor: 'rgba(0,0,0,0.75)',
    borderRadius: 6,
    left: 12,
    padding: 6,
    position: 'absolute',
  },
  devBadgeText: { color: '#fff', fontSize: 11 },
});
