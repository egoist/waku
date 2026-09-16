import type { AgentSession } from '@waku/client';
import * as Haptics from 'expo-haptics';
import { router, useGlobalSearchParams, usePathname } from 'expo-router';
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import {
  ActivityIndicator,
  Alert,
  AppState,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  RefreshControl,
  SectionList,
  StyleSheet,
  Text,
  TextInput,
  View,
  useWindowDimensions,
} from 'react-native';
import { Drawer } from 'react-native-drawer-layout';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { AppSymbol } from '@/components/app-symbol';
import { ConnectionBanner, useConnectionNotice } from '@/components/connection-banner';
import { DaemonPickerSheet } from '@/components/daemon-picker-sheet';
import { GlassSurface } from '@/components/glass-surface';
import { ConnectionStatus, connectionPhaseLabel } from '@/components/connection-status';
import { ProviderIcon } from '@/components/provider-icon';
import { RenameDialog } from '@/components/rename-dialog';
import { TaskRowMenu } from '@/components/task-row-menu';
import { NativeTint, Radius, Spacing } from '@/constants/theme';
import { useTaskState } from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import { useDaemon } from '@/lib/daemon-context';
import { sessionIsRunning } from '@/lib/mobile-runtime';
import { useRuntime } from '@/lib/runtime-context';
import {
  displaySessionTitle,
  groupSessions,
  providerLabel,
  relativeSessionTime,
  type SessionListItem,
} from '@/lib/session-presentation';

const DaemonPickerHeight = 38;
const SearchDockGap = 14;
interface TaskDrawerContextValue {
  openTaskDrawer: () => void;
  closeTaskDrawer: () => void;
}

const TaskDrawerContext = createContext<TaskDrawerContextValue | null>(null);

export function TaskDrawerHost({ children }: { children: ReactNode }) {
  const theme = useTheme();
  const daemon = useDaemon();
  const pathname = usePathname();
  const params = useGlobalSearchParams<{ id?: string | string[] }>();
  const { width } = useWindowDimensions();
  const [open, setOpen] = useState(false);
  const drawerWidth = Math.max(0, Math.min(360, width - 44));
  const drawerEnabled = daemon.phase === 'booting' || daemon.profiles.length > 0;
  const openTaskDrawer = useCallback(() => {
    if (drawerEnabled) setOpen(true);
  }, [drawerEnabled]);
  const closeTaskDrawer = useCallback(() => setOpen(false), []);
  const controls = useMemo(
    () => ({ openTaskDrawer, closeTaskDrawer }),
    [closeTaskDrawer, openTaskDrawer],
  );
  const swipeEnabled = pathname === '/'
    || pathname === '/new-task'
    || pathname.startsWith('/session/');
  const selectedSessionId = pathname.startsWith('/session/')
    ? Array.isArray(params.id) ? params.id[0] : params.id ?? null
    : null;

  useEffect(() => setOpen(false), [drawerEnabled, pathname]);

  return (
    <TaskDrawerContext.Provider value={controls}>
      {drawerEnabled ? (
        <Drawer
          drawerStyle={{ backgroundColor: theme.background, width: drawerWidth }}
          drawerType="back"
          open={open}
          overlayAccessibilityLabel="Close task history"
          overlayStyle={{ backgroundColor: 'rgba(0, 0, 0, 0.18)' }}
          renderDrawerContent={() => (
            <TaskDrawerContent
              drawerWidth={drawerWidth}
              open={open}
              selectedSessionId={selectedSessionId}
              onClose={closeTaskDrawer}
            />
          )}
          swipeEdgeWidth={width}
          swipeEnabled={swipeEnabled}
          onClose={closeTaskDrawer}
          onOpen={openTaskDrawer}>
          {children}
        </Drawer>
      ) : (
        <>{children}</>
      )}
    </TaskDrawerContext.Provider>
  );
}

export function useTaskDrawer(): TaskDrawerContextValue {
  const context = useContext(TaskDrawerContext);
  if (!context) throw new Error('useTaskDrawer must be used inside TaskDrawerHost');
  return context;
}

function TaskDrawerContent({
  drawerWidth,
  open,
  selectedSessionId,
  onClose,
}: {
  drawerWidth: number;
  open: boolean;
  selectedSessionId: string | null;
  onClose: () => void;
}) {
  const theme = useTheme();
  const insets = useSafeAreaInsets();
  const daemon = useDaemon();
  const runtime = useRuntime();
  const taskState = useTaskState();
  const [search, setSearch] = useState('');
  const [refreshing, setRefreshing] = useState(false);
  const [daemonPickerOpen, setDaemonPickerOpen] = useState(false);
  const [renameTarget, setRenameTarget] = useState<AgentSession | null>(null);
  const [now, setNow] = useState(Date.now);
  // Share one clock across visible rows, and park it while the drawer or app
  // is hidden so recency labels add no work to the conversation screen.
  useEffect(() => {
    if (!open) return;
    let timer: ReturnType<typeof setInterval> | undefined;
    const refresh = () => setNow(Date.now());
    const updateClock = (state: string | null) => {
      clearInterval(timer);
      if (state === 'active' || state === null) {
        refresh();
        timer = setInterval(refresh, 30_000);
      }
    };
    updateClock(AppState.currentState);
    const subscription = AppState.addEventListener('change', updateClock);
    return () => {
      clearInterval(timer);
      subscription.remove();
    };
  }, [open]);
  const listExtraData = useMemo(
    () => ({ runtimes: runtime.runtimes, now }),
    [runtime.runtimes, now],
  );
  const visibleSessions = useMemo(() => {
    if (!taskState.data) return [];
    const query = search.trim().toLocaleLowerCase();
    if (!query) return taskState.data.sessions;
    const projects = new Map(taskState.data.projects.map((project) => [project.id, project]));
    return taskState.data.sessions.filter((session) => {
      const project = projects.get(session.project_id);
      return [
        displaySessionTitle(session),
        project?.name,
        project?.path,
        providerLabel(session.provider),
        session.model,
      ].some((value) => value?.toLocaleLowerCase().includes(query));
    });
  }, [search, taskState.data]);
  const sections = useMemo(
    () => taskState.data ? groupSessions(taskState.data.projects, visibleSessions) : [],
    [taskState.data, visibleSessions],
  );
  const showNewTask = useCallback(() => {
    onClose();
    router.dismissTo('/');
  }, [onClose]);
  const showSession = useCallback((sessionId: string) => {
    if (selectedSessionId === sessionId) {
      onClose();
    } else if (selectedSessionId) {
      router.setParams({ id: sessionId });
    } else {
      router.replace({ pathname: '/session/[id]', params: { id: sessionId } });
    }
  }, [onClose, selectedSessionId]);

  function confirmDelete(session: AgentSession) {
    Alert.alert(
      `Delete “${displaySessionTitle(session)}”?`,
      'This removes the task and its transcript from the daemon for every device.',
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Delete',
          style: 'destructive',
          onPress: () => {
            void runtime.deleteSession(session.id)
              .then(() => Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success))
              .catch((cause) => {
                Alert.alert(
                  'Couldn’t delete task',
                  cause instanceof Error ? cause.message : String(cause),
                );
              });
          },
        },
      ],
    );
  }

  async function refreshTasks() {
    setRefreshing(true);
    try {
      if (daemon.phase === 'connected') await taskState.refetch();
      else await daemon.reconnect();
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <View style={[styles.screen, { backgroundColor: theme.background }]}>
      <View
        pointerEvents="box-none"
        style={[styles.daemonFloat, { top: insets.top + 8 }]}>
        <DaemonPill onPress={() => setDaemonPickerOpen(true)} />
      </View>

      <SectionList
        sections={sections}
        extraData={listExtraData}
        keyExtractor={(item) => item.session.id}
        contentContainerStyle={[
          styles.listContent,
          {
            paddingTop: insets.top + DaemonPickerHeight + 20,
          },
          sections.length === 0 && styles.listContentEmpty,
        ]}
        refreshControl={(
          <RefreshControl
            colors={[theme.textTertiary]}
            progressViewOffset={insets.top + DaemonPickerHeight + 12}
            refreshing={refreshing}
            tintColor={theme.textTertiary}
            onRefresh={() => void refreshTasks()}
          />
        )}
        renderSectionHeader={({ section }) => (
          <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>
            {section.title}
          </Text>
        )}
        renderItem={({ item }) => (
          <SessionRow
            drawerWidth={drawerWidth}
            item={item}
            now={now}
            running={runtime.runtimes[item.session.id]?.running ?? sessionIsRunning(item.session)}
            selected={item.session.id === selectedSessionId}
            onDelete={() => confirmDelete(item.session)}
            onRename={() => setRenameTarget(item.session)}
            onSelect={() => showSession(item.session.id)}
          />
        )}
        ListHeaderComponent={<ConnectionBanner />}
        ListEmptyComponent={(
          <TaskListEmpty
            error={taskState.error}
            searching={Boolean(search.trim())}
            onNewTask={showNewTask}
          />
        )}
        showsVerticalScrollIndicator={false}
        stickySectionHeadersEnabled={false}
      />

      {(daemon.profiles.length > 0 || daemon.phase === 'booting') && (
        <KeyboardAvoidingView
          behavior={Platform.OS === 'ios' ? 'position' : undefined}
          keyboardVerticalOffset={SearchDockGap}
          pointerEvents="box-none"
          style={[styles.searchDockAvoider, { bottom: insets.bottom + SearchDockGap }]}>
          <View pointerEvents="box-none" style={styles.searchDock}>
            <GlassSurface interactive style={styles.searchCapsule}>
              <View style={styles.searchCapsuleInner}>
                <AppSymbol
                  name={{ ios: 'magnifyingglass', android: 'search', web: 'search' }}
                  size={17}
                  tintColor={theme.textSecondary}
                />
                <TextInput
                  accessibilityLabel="Search tasks"
                  autoCapitalize="none"
                  autoCorrect={false}
                  placeholder="Search"
                  placeholderTextColor={theme.textTertiary}
                  selectionColor={NativeTint}
                  style={[styles.searchInput, { color: theme.text }]}
                  value={search}
                  onChangeText={setSearch}
                />
                {search.length > 0 && (
                  <Pressable
                    accessibilityLabel="Clear search"
                    accessibilityRole="button"
                    hitSlop={8}
                    onPress={() => setSearch('')}
                    style={({ pressed }) => ({ opacity: pressed ? 0.5 : 1 })}>
                    <AppSymbol
                      name={{ ios: 'xmark.circle.fill', android: 'cancel', web: 'cancel' }}
                      size={16}
                      tintColor={theme.textTertiary}
                    />
                  </Pressable>
                )}
              </View>
            </GlassSurface>
            {daemon.phase === 'connected' && (
              <GlassSurface interactive style={styles.composeButton}>
                <Pressable
                  accessibilityLabel="New task"
                  accessibilityRole="button"
                  hitSlop={6}
                  onPress={showNewTask}
                  style={({ pressed }) => [styles.roundInner, { opacity: pressed ? 0.5 : 1 }]}>
                  <AppSymbol
                    name={{ ios: 'square.and.pencil', android: 'edit_square', web: 'edit' }}
                    size={20}
                    tintColor={theme.text}
                  />
                </Pressable>
              </GlassSurface>
            )}
          </View>
        </KeyboardAvoidingView>
      )}

      {renameTarget && (
        <RenameDialog
          initialValue={displaySessionTitle(renameTarget)}
          onDismiss={() => setRenameTarget(null)}
          onSubmit={(title) => runtime.renameSession(renameTarget.id, title)}
          visible
        />
      )}
      <DaemonPickerSheet
        onDismiss={() => setDaemonPickerOpen(false)}
        visible={daemonPickerOpen}
      />
    </View>
  );
}

function DaemonPill({ onPress }: { onPress: () => void }) {
  const theme = useTheme();
  const daemon = useDaemon();
  return (
    <GlassSurface interactive style={styles.daemonButton}>
      <Pressable
        accessibilityHint="Opens the daemon switcher"
        accessibilityLabel={daemon.activeProfile
          ? `${connectionPhaseLabel(daemon.phase)}: ${daemon.activeProfile.name}`
          : 'Add a daemon'}
        accessibilityRole="button"
        hitSlop={8}
        onPress={onPress}
        style={({ pressed }) => [styles.daemonButtonInner, { opacity: pressed ? 0.62 : 1 }]}>
        {daemon.activeProfile ? <ConnectionStatus compact phase={daemon.phase} /> : (
          <AppSymbol
            name={{ ios: 'plus', android: 'add', web: 'add' }}
            size={14}
            tintColor={theme.text}
          />
        )}
        <Text numberOfLines={1} style={[styles.daemonButtonText, { color: theme.text }]}>
          {daemon.activeProfile?.name ?? 'Add daemon'}
        </Text>
        <AppSymbol
          name={{ ios: 'chevron.down', android: 'keyboard_arrow_down', web: 'keyboard_arrow_down' }}
          size={12}
          tintColor={theme.textTertiary}
        />
      </Pressable>
    </GlassSurface>
  );
}

function TaskListEmpty({
  error,
  searching,
  onNewTask,
}: {
  error: unknown;
  searching: boolean;
  onNewTask: () => void;
}) {
  const theme = useTheme();
  const { phase } = useDaemon();
  const notice = useConnectionNotice();
  // The banner above the list is already explaining the wait.
  if (notice && notice.kind !== 'restored') return null;
  if (phase === 'booting' || phase === 'connecting' || phase === 'reconnecting') {
    return (
      <View style={styles.emptyState}>
        <ActivityIndicator color={theme.textTertiary} />
        <Text style={[styles.emptyTitle, { color: theme.textSecondary }]}>
          {phase === 'reconnecting' ? 'Reconnecting…' : 'Connecting to daemon…'}
        </Text>
      </View>
    );
  }
  if (searching) {
    return (
      <View style={styles.emptyState}>
        <Text style={[styles.emptyTitle, { color: theme.text }]}>No matching tasks</Text>
        <Text style={[styles.emptyBody, { color: theme.textSecondary }]}>Try another title, project, or agent.</Text>
      </View>
    );
  }
  if (error) {
    return (
      <View style={styles.emptyState}>
        <Text style={[styles.emptyTitle, { color: theme.text }]}>Couldn’t load tasks</Text>
        <Text style={[styles.emptyBody, { color: theme.textSecondary }]}>
          {error instanceof Error ? error.message : String(error)}
        </Text>
      </View>
    );
  }
  return (
    <View style={styles.emptyState}>
      <View style={[styles.emptyIcon, { backgroundColor: theme.overlayStrong }]}>
        <AppSymbol
          name={{ ios: 'text.bubble', android: 'chat_bubble', web: 'chat' }}
          size={25}
          tintColor={theme.textTertiary}
        />
      </View>
      <Text style={[styles.emptyTitle, { color: theme.text }]}>No tasks yet</Text>
      <Text style={[styles.emptyBody, { color: theme.textSecondary }]}>
        Start an agent on anything — a bug, a feature, a question about the code.
      </Text>
      {phase === 'connected' && (
        <Pressable
          accessibilityRole="button"
          onPress={onNewTask}
          style={({ pressed }) => [
            styles.emptyAction,
            { backgroundColor: theme.inverse, opacity: pressed ? 0.7 : 1 },
          ]}>
          <Text style={[styles.emptyActionText, { color: theme.onInverse }]}>New task</Text>
        </Pressable>
      )}
    </View>
  );
}

function SessionRow({
  drawerWidth,
  item,
  now,
  running,
  selected,
  onDelete,
  onRename,
  onSelect,
}: {
  drawerWidth: number;
  item: SessionListItem;
  now: number;
  running: boolean;
  selected: boolean;
  onDelete: () => void;
  onRename: () => void;
  onSelect: () => void;
}) {
  const theme = useTheme();
  const rowWidth = Math.max(0, drawerWidth - 24);
  const session = item.session;
  const lastReplyTime = session.last_reply_at == null
    ? null
    : relativeSessionTime(session.last_reply_at, now);
  return (
    <TaskRowMenu
      accessibilityLabel={`${displaySessionTitle(session)}, ${providerLabel(session.provider)} in ${item.projectName}${running ? ', Running' : ''}${lastReplyTime ? `, Last reply: ${lastReplyTime}` : ''}`}
      onDelete={onDelete}
      onRename={onRename}
      onSelect={onSelect}
      renderTrigger={(pressed) => (
        <View
          style={[
            styles.sessionRow,
            {
              backgroundColor: pressed
                ? theme.surfaceMuted
                : selected ? theme.backgroundSelected : 'transparent',
              width: rowWidth,
            },
          ]}>
          <View style={styles.sessionHeading}>
            <Text numberOfLines={1} style={[styles.sessionTitle, { color: theme.text }]}>
              {displaySessionTitle(session)}
            </Text>
            {running && (
              <ActivityIndicator
                accessibilityLabel="Running"
                color={theme.textTertiary}
                size="small"
                style={styles.sessionSpinner}
              />
            )}
          </View>
          <View style={styles.sessionMetadata}>
            <ProviderIcon color={theme.textTertiary} provider={session.provider} size={12} />
            <Text
              numberOfLines={1}
              style={[styles.sessionProject, { color: theme.textTertiary }]}>
              {item.projectName}
            </Text>
            {lastReplyTime !== null && (
              <Text
                numberOfLines={1}
                style={[styles.sessionTime, { color: theme.textTertiary }]}>
                {lastReplyTime}
              </Text>
            )}
          </View>
        </View>
      )}
      selected={selected}
      style={[styles.sessionMenu, { width: rowWidth }]}
    />
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  daemonFloat: {
    left: 12,
    position: 'absolute',
    zIndex: 30,
  },
  roundInner: { alignItems: 'center', flex: 1, justifyContent: 'center' },
  searchDockAvoider: {
    left: 0,
    position: 'absolute',
    right: 0,
    zIndex: 20,
  },
  searchDock: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 10,
    paddingHorizontal: Spacing.three,
  },
  searchCapsule: { borderRadius: Radius.pill, flex: 1 },
  searchCapsuleInner: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 9,
    minHeight: 50,
    paddingHorizontal: 16,
  },
  searchInput: { flex: 1, fontSize: 16.5, paddingVertical: 10 },
  composeButton: { borderRadius: Radius.pill, height: 50, width: 50 },
  daemonButton: { borderRadius: Radius.pill, maxWidth: 176 },
  daemonButtonInner: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 6,
    minHeight: DaemonPickerHeight,
    paddingHorizontal: 12,
  },
  daemonButtonText: { flexShrink: 1, fontSize: 13, fontWeight: '600' },
  listContent: { paddingBottom: 96 },
  listContentEmpty: { flexGrow: 1 },
  sectionTitle: {
    fontSize: 13,
    fontWeight: '500',
    letterSpacing: 0,
    marginBottom: 4,
    marginHorizontal: 24,
    marginTop: 14,
  },
  emptyState: {
    alignItems: 'center',
    flex: 1,
    justifyContent: 'center',
    minHeight: 360,
    paddingHorizontal: 40,
  },
  emptyIcon: {
    alignItems: 'center',
    borderRadius: 20,
    height: 64,
    justifyContent: 'center',
    marginBottom: 18,
    width: 64,
  },
  emptyTitle: { fontSize: 17, fontWeight: '700', textAlign: 'center' },
  emptyBody: { fontSize: 14, lineHeight: 20, marginTop: 7, maxWidth: 320, textAlign: 'center' },
  emptyAction: {
    borderRadius: Radius.pill,
    justifyContent: 'center',
    marginTop: 18,
    minHeight: 42,
    paddingHorizontal: 18,
  },
  emptyActionText: { fontSize: 14, fontWeight: '700' },
  sessionMenu: { height: 62, marginHorizontal: 12 },
  sessionRow: {
    borderRadius: 10,
    gap: 3,
    height: 62,
    justifyContent: 'center',
    paddingHorizontal: 12,
  },
  sessionHeading: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  sessionMetadata: { alignItems: 'center', flexDirection: 'row', gap: 5 },
  sessionProject: { flex: 1, fontSize: 12.5, lineHeight: 17 },
  sessionTime: { flexShrink: 0, fontSize: 12.5, lineHeight: 17, marginLeft: 3 },
  sessionSpinner: { height: 14, transform: [{ scale: 0.72 }], width: 14 },
  sessionTitle: {
    flex: 1,
    fontSize: 16.5,
    fontWeight: '400',
    letterSpacing: -0.2,
    lineHeight: 22,
  },
});
