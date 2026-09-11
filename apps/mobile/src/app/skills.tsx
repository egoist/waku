import type { SkillEntry } from '@waku/client';
import * as Haptics from 'expo-haptics';
import { Stack } from 'expo-router';
import { useState } from 'react';
import {
  ActivityIndicator,
  Alert,
  ScrollView,
  StyleSheet,
  Switch,
  Text,
  View,
} from 'react-native';

import { AppPressable } from '@/components/app-pressable';

import { NativeTint, Radius, Spacing } from '@/constants/theme';
import {
  useSetSkillsEnabled,
  useSkills,
  useTaskState,
  useTrashSkills,
} from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import {
  groupSkillsByProject,
  skillEnabled,
  skillInstallDirs,
  skillScopeLabel,
  skillSizeLabel,
  skillSourcesLabel,
  skillSummary,
} from '@/lib/skill-presentation';

export default function SkillsScreen() {
  const theme = useTheme();
  const taskState = useTaskState();
  const projects = taskState.data?.projects ?? [];
  const skills = useSkills(projects);
  const setEnabled = useSetSkillsEnabled();
  const trash = useTrashSkills();
  const [localError, setLocalError] = useState<string | null>(null);
  const groups = groupSkillsByProject(skills.data?.skills ?? []);
  const busy = setEnabled.isPending || trash.isPending;

  async function toggle(entry: SkillEntry, enabled: boolean) {
    setLocalError(null);
    try {
      await setEnabled.mutateAsync({ dirs: skillInstallDirs(entry), enabled });
      await Haptics.selectionAsync();
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  function confirmTrash(entry: SkillEntry) {
    Alert.alert(
      `Move “${entry.name}” to the trash?`,
      'This removes the skill for every device. The daemon host keeps it in its trash.',
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Move to trash',
          style: 'destructive',
          onPress: () => {
            void trash.mutateAsync(skillInstallDirs(entry)).catch((cause: unknown) => {
              setLocalError(cause instanceof Error ? cause.message : String(cause));
            });
          },
        },
      ],
    );
  }

  return (
    <View style={[styles.screen, { backgroundColor: theme.background }]}>
      <Stack.Screen options={{ title: 'Skills' }} />
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        contentContainerStyle={styles.content}>
        {!skills.data ? (
          <View style={styles.loading}>
            <ActivityIndicator color={theme.textTertiary} />
            <Text style={[styles.loadingLabel, { color: theme.textTertiary }]}>
              {skills.isPending ? 'Reading skills…' : 'No skills yet'}
            </Text>
          </View>
        ) : (
          groups.map((group) => (
            <View key={group.project ?? 'user'}>
              <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>
                {group.project ?? 'User skills'}
              </Text>
              <View
                style={[
                  styles.card,
                  { backgroundColor: theme.surface, borderColor: theme.border },
                ]}>
                {group.skills.map((entry, index) => (
                  <AppPressable
                    accessibilityHint="Long press to move to the trash"
                    delayLongPress={400}
                    key={`${entry.name}:${entry.rowKey}`}
                    onLongPress={() => confirmTrash(entry)}
                    style={({ pressed }) => [
                      styles.row,
                      index > 0
                        ? {
                            borderTopColor: theme.separator,
                            borderTopWidth: StyleSheet.hairlineWidth,
                          }
                        : null,
                      pressed ? { backgroundColor: theme.surfaceMuted } : null,
                    ]}>
                    <View style={styles.rowText}>
                      <Text numberOfLines={1} style={[styles.name, { color: theme.text }]}>
                        {entry.name}
                      </Text>
                      {entry.description ? (
                        <Text
                          numberOfLines={2}
                          style={[styles.description, { color: theme.textSecondary }]}>
                          {entry.description}
                        </Text>
                      ) : null}
                      <Text
                        numberOfLines={1}
                        style={[styles.meta, { color: theme.textTertiary }]}>
                        {[
                          skillSourcesLabel(entry),
                          skillScopeLabel(entry),
                          skillSizeLabel(entry),
                        ].join(' · ')}
                      </Text>
                    </View>
                    <Switch
                      accessibilityLabel={`${entry.name} ${skillEnabled(entry) ? 'enabled' : 'disabled'}`}
                      disabled={busy || !entry.installs.length}
                      ios_backgroundColor={theme.overlayStrong}
                      thumbColor={theme.surface}
                      trackColor={{ false: theme.overlayStrong, true: NativeTint }}
                      value={skillEnabled(entry)}
                      onValueChange={(enabled) => void toggle(entry, enabled)}
                    />
                  </AppPressable>
                ))}
              </View>
            </View>
          ))
        )}

        {skills.data ? (
          <Text style={[styles.footer, { color: theme.textGhost }]}>
            {skillSummary(skills.data.skills)}
          </Text>
        ) : null}
        {localError ? (
          <Text style={[styles.error, { color: theme.danger }]}>{localError}</Text>
        ) : null}
        <Text style={[styles.footer, { color: theme.textGhost }]}>
          Skills live on the daemon host. Long-press one to move it to the trash, which the
          desktop can undo.
        </Text>
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  content: { paddingBottom: 36, paddingHorizontal: Spacing.three },
  loading: { alignItems: 'center', gap: 10, paddingTop: 60 },
  loadingLabel: { fontSize: 13 },
  card: {
    borderWidth: StyleSheet.hairlineWidth,
    borderRadius: Radius.medium,
    paddingHorizontal: 14,
  },
  sectionTitle: {
    fontSize: 13,
    fontWeight: '500',
    marginBottom: 7,
    marginLeft: 12,
    marginTop: 18,
    textTransform: 'uppercase',
  },
  row: { flexDirection: 'row', gap: 10, paddingVertical: 11 },
  rowText: { flex: 1, gap: 2 },
  name: { fontSize: 15.5, fontWeight: '500' },
  description: { fontSize: 12.5, lineHeight: 17 },
  meta: { fontSize: 11.5, marginTop: 2 },
  footer: { fontSize: 12, lineHeight: 17, marginLeft: 12, marginTop: 8 },
  error: { fontSize: 12.5, lineHeight: 17, marginLeft: 12, marginTop: 12 },
});
