import type { AgentSession } from '@waku/client';
import * as Haptics from 'expo-haptics';
import { router, useGlobalSearchParams, usePathname } from 'expo-router';
import {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import {
  ActivityIndicator,
  Alert,
  Keyboard,
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
import { useSessionMessageSearch, useTaskState } from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import { useDaemon } from '@/lib/daemon-context';
import { sessionIsRunning } from '@/lib/mobile-runtime';
import { useRuntime } from '@/lib/runtime-context';
import {
  displaySessionTitle,
  groupSessions,
  messageSearchRows,
  providerLabel,
  type MessageSearchRow,
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
  // While the search field is focused the keyboard can swallow the first tap on
  // the drawer overlay, leaving it uncloseable; this adds a one-tap catcher.
  const [searchFocused, setSearchFocused] = useState(false);
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

  const drawerScene = <View style={styles.drawerScene}>{children}</View>;

  return (
    <TaskDrawerContext.Provider value={controls}>
      <View style={styles.drawerHost}>
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
                selectedSessionId={selectedSessionId}
                onClose={closeTaskDrawer}
                onSearchFocus={setSearchFocused}
              />
            )}
            swipeEdgeWidth={width}
            swipeEnabled={swipeEnabled}
            onClose={closeTaskDrawer}
            onOpen={openTaskDrawer}>
            {drawerScene}
          </Drawer>
        ) : (
          drawerScene
        )}
        {open && searchFocused && (
          <Pressable
            accessibilityLabel="Close task history"
            accessibilityRole="button"
            onPress={() => {
              Keyboard.dismiss();
              closeTaskDrawer();
            }}
            style={[styles.drawerCloseCatcher, { left: drawerWidth }]}
          />
        )}
      </View>
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
  selectedSessionId,
  onClose,
  onSearchFocus,
}: {
  drawerWidth: number;
  selectedSessionId: string | null;
  onClose: () => void;
  onSearchFocus?: (focused: boolean) => void;
}) {
  const theme = useTheme();
  const insets = useSafeAreaInsets();
  const daemon = useDaemon();
  const runtime = useRuntime();
  const taskState = useTaskState();
  const [search, setSearch] = useState('');
  const [refreshing, setRefreshing] = useState(false);
  // Every keystroke would otherwise be a full transcript scan on the daemon.
  const [messageQuery, setMessageQuery] = useState('');
  useEffect(() => {
    const timer = setTimeout(() => setMessageQuery(search.trim()), 180);
    return () => clearTimeout(timer);
  }, [search]);
  const messageSearch = useSessionMessageSearch(messageQuery);
  const [daemonPickerOpen, setDaemonPickerOpen] = useState(false);
  const [renameTarget, setRenameTarget] = useState<AgentSession | null>(null);
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
  const messageRows = useMemo(
    () => (messageQuery.trim() && messageSearch.data)
      ? messageSearchRows(messageSearch.data, taskState.data?.sessions ?? [])
      : [],
    [messageQuery, messageSearch.data, taskState.data?.sessions],
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

  // Streaming mutates `runtime` every tick, so capture it in a ref to keep the
  // row callbacks stable. Without this, every visible row re-renders on each
  // stream commit — the drawer list is the heaviest thing on screen.
  const runtimeRef = useRef(runtime);
  runtimeRef.current = runtime;
  const handleSelect = useCallback((sessionId: string) => showSession(sessionId), [showSession]);
  const handleRename = useCallback((session: AgentSession) => setRenameTarget(session), []);
  const handleDelete = useCallback((session: AgentSession) => {
    Alert.alert(
      `Delete “${displaySessionTitle(session)}”?`,
      'This removes the task and its transcript from the daemon for every device.',
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Delete',
          style: 'destructive',
          onPress: () => {
            void runtimeRef.current.deleteSession(session.id)
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
  }, []);

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
        extraData={runtime.runtimes}
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
            running={Boolean(runtime.runtimes[item.session.id]?.running ?? sessionIsRunning(item.session))}
            selected={item.session.id === selectedSessionId}
            onDelete={handleDelete}
            onRename={handleRename}
            onSelect={handleSelect}
          />
        )}
        ListHeaderComponent={(
          <>
            <ConnectionBanner />
            {messageRows.length ? (
              <View style={styles.messageResults}>
                <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>
                  Messages
                </Text>
                {messageRows.map((row, index) => (
                  <MessageResultRow
                    key={`${row.session.id}:${index}`}
                    onSelect={handleSelect}
                    row={row}
                  />
                ))}
              </View>
            ) : null}
          </>
        )}
        ListEmptyComponent={(
          <TaskListEmpty
            error={taskState.error}
            searching={Boolean(search.trim()) && !messageRows.length}
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
                  onFocus={() => onSearchFocus?.(true)}
                  onBlur={() => onSearchFocus?.(false)}
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

/** A transcript hit. The title is what you recognise; the snippet is the
 * evidence, so both are shown and neither is truncated to one line only. */
const MessageResultRow = memo(function MessageResultRow({
  onSelect,
  row,
}: {
  onSelect: (sessionId: string) => void;
  row: MessageSearchRow;
}) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityLabel={`${displaySessionTitle(row.session)}: ${row.snippet}`}
      accessibilityRole="button"
      onPress={() => onSelect(row.session.id)}
      style={({ pressed }) => [
        styles.messageRow,
        { backgroundColor: pressed ? theme.surfaceMuted : 'transparent' },
      ]}>
      <View style={styles.messageHeading}>
        <ProviderIcon color={theme.textTertiary} provider={row.session.provider} size={11} />
        <Text numberOfLines={1} style={[styles.messageTitle, { color: theme.text }]}>
          {displaySessionTitle(row.session)}
        </Text>
      </View>
      <Text numberOfLines={2} style={[styles.messageSnippet, { color: theme.textSecondary }]}>
        {row.snippet}
      </Text>
    </Pressable>
  );
});

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

const SessionRow = memo(function SessionRow({
  drawerWidth,
  item,
  running,
  selected,
  onDelete,
  onRename,
  onSelect,
}: {
  drawerWidth: number;
  item: SessionListItem;
  running: boolean;
  selected: boolean;
  onDelete: (session: AgentSession) => void;
  onRename: (session: AgentSession) => void;
  onSelect: (sessionId: string) => void;
}) {
  const theme = useTheme();
  const rowWidth = Math.max(0, drawerWidth - 24);
  const session = item.session;
  return (
    <TaskRowMenu
      accessibilityLabel={`${displaySessionTitle(session)}, ${providerLabel(session.provider)} in ${item.projectName}${running ? ', Running' : ''}`}
      onDelete={() => onDelete(session)}
      onRename={() => onRename(session)}
      onSelect={() => onSelect(session.id)}
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
          </View>
        </View>
      )}
      selected={selected}
      style={[styles.sessionMenu, { width: rowWidth }]}
    />
  );
});

const styles = StyleSheet.create({
  screen: { flex: 1 },
  drawerHost: { flex: 1 },
  drawerScene: { flex: 1 },
  // Rendered above the drawer, covering only the strip of screen beside the
  // drawer (where the dim overlay lives) so a single tap blurs search + closes.
  drawerCloseCatcher: {
    bottom: 0,
    position: 'absolute',
    right: 0,
    top: 0,
    zIndex: 1000,
  },
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
  messageResults: { marginBottom: 6 },
  messageRow: {
    borderRadius: 10,
    gap: 2,
    marginHorizontal: 12,
    paddingHorizontal: 12,
    paddingVertical: 8,
  },
  messageHeading: { alignItems: 'center', flexDirection: 'row', gap: 5 },
  messageTitle: { flex: 1, fontSize: 14.5, fontWeight: '500' },
  messageSnippet: { fontSize: 12.5, lineHeight: 17 },
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
  sessionSpinner: { height: 14, transform: [{ scale: 0.72 }], width: 14 },
  sessionTitle: {
    flex: 1,
    fontSize: 16.5,
    fontWeight: '400',
    letterSpacing: -0.2,
    lineHeight: 22,
  },
});
