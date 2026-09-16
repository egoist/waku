import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { Project, WorkingTreeEntry } from '@waku/client';
import * as Crypto from 'expo-crypto';
import * as Haptics from 'expo-haptics';
import { useEffect, useMemo, useState } from 'react';
import {
  ActivityIndicator,
  FlatList,
  KeyboardAvoidingView,
  Modal,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import { SafeAreaView } from 'react-native-safe-area-context';

import { AppSymbol } from './app-symbol';
import { Radius, Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import {
  browseDaemonDirectory,
  createProject,
  createProjectlessWorkspace,
  daemonKeys,
  persistProject,
  type TaskState,
} from '@/lib/daemon-api';
import { useDaemon } from '@/lib/daemon-context';

export function RemoteProjectPicker({
  visible,
  onDismiss,
  onSelect,
}: {
  visible: boolean;
  onDismiss: () => void;
  onSelect: (project: Project) => void;
}) {
  const theme = useTheme();
  const daemon = useDaemon();
  const queryClient = useQueryClient();
  const profileId = daemon.activeProfile?.id ?? 'disconnected';
  const [path, setPath] = useState<string | null>(null);
  const [pathDraft, setPathDraft] = useState('');
  const [submitting, setSubmitting] = useState<'folder' | 'empty' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const directory = useQuery({
    queryKey: daemonKeys.directory(profileId, path),
    queryFn: () => {
      if (!daemon.client) throw new Error('Waku daemon is disconnected');
      return browseDaemonDirectory(daemon.client, path);
    },
    enabled: visible && daemon.phase === 'connected' && Boolean(daemon.client),
    staleTime: 10_000,
  });
  const folders = useMemo(
    () => (directory.data?.entries ?? []).filter((entry) => entry.isDir),
    [directory.data?.entries],
  );

  useEffect(() => {
    setPath(null);
    setPathDraft('');
    setError(null);
  }, [profileId]);

  useEffect(() => {
    if (directory.data?.path) setPathDraft(directory.data.path);
  }, [directory.data?.path]);

  function visit(next: string | null) {
    if (submitting) return;
    setError(null);
    setPath(next);
  }

  async function saveProject(project: Project) {
    if (!daemon.client) throw new Error('Waku daemon is disconnected');
    const saved = await persistProject(daemon.client, project);
    queryClient.setQueryData<TaskState>(daemonKeys.taskState(profileId), saved.taskState);
    onSelect(saved.project);
    onDismiss();
    await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
  }

  async function addFolder() {
    const target = directory.data?.path;
    if (!target || submitting) return;
    setSubmitting('folder');
    setError(null);
    try {
      await saveProject(createProject(target, Crypto.randomUUID()));
    } catch (cause) {
      setError(errorMessage(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setSubmitting(null);
    }
  }

  async function addEmptyWorkspace() {
    if (!daemon.client || submitting) return;
    setSubmitting('empty');
    setError(null);
    try {
      const cwd = await createProjectlessWorkspace(daemon.client);
      await saveProject({ ...createProject(cwd, Crypto.randomUUID()), name: 'No project' });
    } catch (cause) {
      setError(errorMessage(cause));
      await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error);
    } finally {
      setSubmitting(null);
    }
  }

  return (
    <Modal
      animationType="slide"
      onRequestClose={onDismiss}
      presentationStyle="pageSheet"
      visible={visible}>
      <KeyboardAvoidingView
        behavior={Platform.OS === 'ios' ? 'padding' : undefined}
        style={[styles.screen, { backgroundColor: theme.background }]}>
        <SafeAreaView edges={['top', 'bottom']} style={styles.screen}>
          <View style={[styles.header, { borderBottomColor: theme.separator }]}>
            <Pressable
              accessibilityRole="button"
              disabled={Boolean(submitting)}
              hitSlop={8}
              onPress={onDismiss}
              style={({ pressed }) => [styles.headerButton, { opacity: pressed ? 0.5 : 1 }]}>
              <Text style={[styles.headerButtonText, { color: theme.accent }]}>Cancel</Text>
            </Pressable>
            <Text style={[styles.title, { color: theme.text }]}>Choose Project</Text>
            <Pressable
              accessibilityLabel="Add current folder"
              accessibilityRole="button"
              disabled={!directory.data?.path || Boolean(submitting)}
              hitSlop={8}
              onPress={() => void addFolder()}
              style={({ pressed }) => [styles.headerButton, styles.headerButtonTrailing, { opacity: pressed ? 0.5 : 1 }]}>
              {submitting === 'folder'
                ? <ActivityIndicator color={theme.accent} size="small" />
                : <Text style={[styles.headerButtonText, { color: directory.data?.path ? theme.accent : theme.textTertiary }]}>Add</Text>}
            </Pressable>
          </View>

          <View style={styles.pathArea}>
            <View style={[styles.pathField, { backgroundColor: theme.surface }]}>
              <AppSymbol
                name={{ ios: 'folder', android: 'folder', web: 'folder' }}
                size={16}
                tintColor={theme.textTertiary}
              />
              <TextInput
                accessibilityLabel="Path on daemon host"
                autoCapitalize="none"
                autoCorrect={false}
                onChangeText={setPathDraft}
                onSubmitEditing={() => visit(pathDraft.trim() || null)}
                placeholder="Path on daemon host"
                placeholderTextColor={theme.textTertiary}
                returnKeyType="go"
                selectTextOnFocus
                spellCheck={false}
                style={[styles.pathInput, { color: theme.text }]}
                value={pathDraft}
              />
              <Pressable
                accessibilityLabel="Open entered path"
                accessibilityRole="button"
                disabled={!pathDraft.trim() || Boolean(submitting)}
                hitSlop={8}
                onPress={() => visit(pathDraft.trim() || null)}
                style={({ pressed }) => [styles.goButton, { opacity: pressed ? 0.45 : 1 }]}>
                <AppSymbol
                  name={{ ios: 'arrow.right.circle.fill', android: 'arrow_circle_right', web: 'arrow_circle_right' }}
                  size={22}
                  tintColor={pathDraft.trim() ? theme.accent : theme.textTertiary}
                />
              </Pressable>
            </View>
            <View style={styles.locations}>
              <LocationButton
                label="Home"
                onPress={() => visit(directory.data?.home ?? null)}
              />
              <LocationButton
                disabled={!directory.data?.parent}
                icon={{ ios: 'arrow.up', android: 'arrow_upward', web: 'arrow_upward' }}
                label="Parent"
                onPress={() => visit(directory.data?.parent ?? null)}
              />
              <LocationButton
                disabled={!directory.data?.filesystem_root}
                icon={{ ios: 'internaldrive', android: 'hard_drive', web: 'hard_drive' }}
                label="File system"
                onPress={() => visit(directory.data?.filesystem_root ?? null)}
              />
            </View>
          </View>

          {error || directory.error ? (
            <View accessibilityLiveRegion="polite" style={[styles.error, { backgroundColor: theme.accentSoft }]}>
              <Text style={[styles.errorText, { color: theme.danger }]}>
                {error ?? errorMessage(directory.error)}
              </Text>
            </View>
          ) : null}

          <FlatList
            data={folders}
            keyExtractor={(item) => item.absolutePath}
            contentContainerStyle={folders.length ? styles.list : styles.emptyList}
            keyboardDismissMode="interactive"
            keyboardShouldPersistTaps="handled"
            ListEmptyComponent={(
              <DirectoryEmpty loading={directory.isPending || directory.isFetching} />
            )}
            renderItem={({ item }) => <FolderRow entry={item} onOpen={() => visit(item.absolutePath)} />}
            showsVerticalScrollIndicator={false}
          />

          <View style={[styles.footer, { borderTopColor: theme.separator }]}>
            <Pressable
              accessibilityHint="Creates a private workspace on the daemon host"
              accessibilityRole="button"
              disabled={Boolean(submitting)}
              onPress={() => void addEmptyWorkspace()}
              style={({ pressed }) => [styles.emptyWorkspaceButton, { opacity: pressed ? 0.5 : 1 }]}>
              {submitting === 'empty' && <ActivityIndicator color={theme.textSecondary} size="small" />}
              <Text style={[styles.emptyWorkspaceText, { color: theme.textSecondary }]}>New empty workspace</Text>
            </Pressable>
            <Text numberOfLines={1} style={[styles.footerPath, { color: theme.textTertiary }]}>
              {directory.data?.path ?? 'Loading folder…'}
            </Text>
          </View>
        </SafeAreaView>
      </KeyboardAvoidingView>
    </Modal>
  );
}

function LocationButton({
  label,
  icon = { ios: 'house', android: 'home', web: 'home' },
  disabled = false,
  onPress,
}: {
  label: string;
  icon?: Parameters<typeof AppSymbol>[0]['name'];
  disabled?: boolean;
  onPress: () => void;
}) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.locationButton,
        { backgroundColor: theme.surfaceMuted, opacity: disabled ? 0.35 : pressed ? 0.55 : 1 },
      ]}>
      <AppSymbol name={icon} size={13} tintColor={theme.textSecondary} />
      <Text style={[styles.locationText, { color: theme.textSecondary }]}>{label}</Text>
    </Pressable>
  );
}

function FolderRow({ entry, onOpen }: { entry: WorkingTreeEntry; onOpen: () => void }) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityHint="Opens this folder"
      accessibilityRole="button"
      onPress={onOpen}
      style={({ pressed }) => [
        styles.folderRow,
        { backgroundColor: pressed ? theme.backgroundSelected : theme.surface },
      ]}>
      <View style={[styles.folderIcon, { backgroundColor: theme.surfaceMuted }]}>
        <AppSymbol
          name={{ ios: 'folder.fill', android: 'folder', web: 'folder' }}
          size={18}
          tintColor={theme.textSecondary}
        />
      </View>
      <Text numberOfLines={1} style={[styles.folderName, { color: theme.text }]}>{entry.name}</Text>
      <AppSymbol
        name={{ ios: 'chevron.right', android: 'chevron_right', web: 'chevron_right' }}
        size={14}
        tintColor={theme.textTertiary}
      />
    </Pressable>
  );
}

function DirectoryEmpty({ loading }: { loading: boolean }) {
  const theme = useTheme();
  return (
    <View style={styles.empty}>
      {loading ? <ActivityIndicator color={theme.textTertiary} /> : (
        <View style={[styles.emptyIcon, { backgroundColor: theme.surfaceMuted }]}>
          <AppSymbol
            name={{ ios: 'folder', android: 'folder_open', web: 'folder_open' }}
            size={24}
            tintColor={theme.textTertiary}
          />
        </View>
      )}
      <Text style={[styles.emptyTitle, { color: theme.textSecondary }]}>
        {loading ? 'Loading folder…' : 'No folders here'}
      </Text>
    </View>
  );
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error && cause.message.trim() ? cause.message : String(cause);
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  header: { alignItems: 'center', borderBottomWidth: StyleSheet.hairlineWidth, flexDirection: 'row', minHeight: 52, paddingHorizontal: 8 },
  headerButton: { justifyContent: 'center', minHeight: 44, minWidth: 64, paddingHorizontal: 8 },
  headerButtonTrailing: { alignItems: 'flex-end' },
  headerButtonText: { fontSize: 16, fontWeight: '600' },
  title: { flex: 1, fontSize: 17, fontWeight: '700', textAlign: 'center' },
  pathArea: { gap: 9, paddingHorizontal: Spacing.three, paddingTop: 13 },
  pathField: { alignItems: 'center', borderRadius: Radius.medium, flexDirection: 'row', minHeight: 45, paddingLeft: 12, paddingRight: 7 },
  pathInput: { flex: 1, fontSize: 14, minHeight: 44, paddingHorizontal: 9, paddingVertical: 8 },
  goButton: { alignItems: 'center', height: 36, justifyContent: 'center', width: 36 },
  locations: { flexDirection: 'row', gap: 7 },
  locationButton: { alignItems: 'center', borderRadius: Radius.pill, flexDirection: 'row', gap: 5, minHeight: 32, paddingHorizontal: 10 },
  locationText: { fontSize: 11.5, fontWeight: '600' },
  error: { borderRadius: Radius.medium, marginHorizontal: Spacing.three, marginTop: 11, padding: 10 },
  errorText: { fontSize: 12.5, fontWeight: '600', lineHeight: 17 },
  list: { gap: 7, padding: Spacing.three, paddingBottom: 28 },
  emptyList: { flexGrow: 1 },
  folderRow: { alignItems: 'center', borderRadius: Radius.medium, flexDirection: 'row', gap: 10, minHeight: 54, paddingHorizontal: 11 },
  folderIcon: { alignItems: 'center', borderRadius: Radius.small, height: 34, justifyContent: 'center', width: 34 },
  folderName: { flex: 1, fontSize: 14.5, fontWeight: '600' },
  empty: { alignItems: 'center', flex: 1, justifyContent: 'center', minHeight: 220 },
  emptyIcon: { alignItems: 'center', borderRadius: 18, height: 58, justifyContent: 'center', width: 58 },
  emptyTitle: { fontSize: 14, fontWeight: '600', marginTop: 12 },
  footer: { alignItems: 'center', borderTopWidth: StyleSheet.hairlineWidth, flexDirection: 'row', gap: 12, minHeight: 58, paddingHorizontal: Spacing.three },
  emptyWorkspaceButton: { alignItems: 'center', flexDirection: 'row', gap: 6, minHeight: 42 },
  emptyWorkspaceText: { fontSize: 12.5, fontWeight: '600' },
  footerPath: { flex: 1, fontSize: 10.5, textAlign: 'right' },
});
