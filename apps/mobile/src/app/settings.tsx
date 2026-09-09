import type { ProviderKind } from '@waku/client';
import { Stack } from 'expo-router';
import { useState } from 'react';
import {
  ActivityIndicator,
  ScrollView,
  StyleSheet,
  Switch,
  Text,
  View,
} from 'react-native';

import { ProviderIcon } from '@/components/provider-icon';
import { NativeTint, Radius, Spacing } from '@/constants/theme';
import {
  PROVIDERS,
  useDaemonSettings,
  useUpdateDaemonSettings,
} from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import {
  orderedProviderToggles,
  withComputerUse,
  withProviderDisabled,
} from '@/lib/daemon-settings';
import { providerLabel } from '@/lib/session-presentation';

export default function SettingsScreen() {
  const theme = useTheme();
  const settings = useDaemonSettings();
  const update = useUpdateDaemonSettings();
  const [localError, setLocalError] = useState<string | null>(null);
  const toggles = orderedProviderToggles(settings.data, PROVIDERS);
  const pending = update.isPending;

  async function apply(next: Parameters<typeof update.mutateAsync>[0]) {
    setLocalError(null);
    try {
      await update.mutateAsync(next);
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  function toggleProvider(provider: ProviderKind, disabled: boolean) {
    if (!settings.data) return;
    void apply(withProviderDisabled(settings.data, provider, disabled));
  }

  return (
    <View style={[styles.screen, { backgroundColor: theme.background }]}>
      <Stack.Screen options={{ title: 'Settings' }} />
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        contentContainerStyle={styles.content}>
        {!settings.data ? (
          <View style={styles.loading}>
            <ActivityIndicator color={theme.textTertiary} />
          </View>
        ) : (
          <>
            <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>Agents</Text>
            <View
              style={[styles.card, { backgroundColor: theme.surface, borderColor: theme.border }]}>
              {toggles.map(({ provider, disabled }, index) => (
                <View
                  key={provider}
                  style={[
                    styles.row,
                    index > 0 ? { borderTopColor: theme.separator, borderTopWidth: StyleSheet.hairlineWidth } : null,
                  ]}>
                  <ProviderIcon provider={provider} size={15} />
                  <Text style={[styles.rowLabel, { color: theme.text }]}>
                    {providerLabel(provider)}
                  </Text>
                  <Switch
                    accessibilityLabel={`${providerLabel(provider)} ${disabled ? 'disabled' : 'enabled'}`}
                    disabled={pending}
                    ios_backgroundColor={theme.overlayStrong}
                    thumbColor={theme.surface}
                    trackColor={{ false: theme.overlayStrong, true: NativeTint }}
                    value={!disabled}
                    onValueChange={(enabled) => toggleProvider(provider, !enabled)}
                  />
                </View>
              ))}
            </View>
            <Text style={[styles.footer, { color: theme.textTertiary }]}>
              A disabled agent disappears from the picker on every device, not just this one.
            </Text>

            <Text style={[styles.sectionTitle, { color: theme.textTertiary }]}>
              Computer use
            </Text>
            <View
              style={[styles.card, { backgroundColor: theme.surface, borderColor: theme.border }]}>
              <View style={styles.row}>
                <Text style={[styles.rowLabel, { color: theme.text }]}>
                  Allow the agent to drive the daemon host
                </Text>
                <Switch
                  accessibilityLabel="Computer use"
                  disabled={pending}
                  ios_backgroundColor={theme.overlayStrong}
                  thumbColor={theme.surface}
                  trackColor={{ false: theme.overlayStrong, true: NativeTint }}
                  value={settings.data.computer_use_enabled}
                  onValueChange={(enabled) => void apply(withComputerUse(settings.data!, enabled))}
                />
              </View>
            </View>
            <Text style={[styles.footer, { color: theme.textTertiary }]}>
              Computer use runs on the daemon host. Which apps it may drive is managed from the
              desktop.
            </Text>
          </>
        )}

        {localError ? (
          <Text style={[styles.error, { color: theme.danger }]}>{localError}</Text>
        ) : null}

        <Text style={[styles.footer, { color: theme.textGhost }]}>
          Settings are stored on the daemon and shared by every connected device.
        </Text>
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  content: { paddingBottom: 36, paddingHorizontal: Spacing.three },
  loading: { paddingTop: 60 },
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
  row: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 10,
    minHeight: 48,
  },
  rowLabel: { flex: 1, fontSize: 15.5 },
  footer: { fontSize: 12, lineHeight: 17, marginLeft: 12, marginTop: 8 },
  error: { fontSize: 12.5, lineHeight: 17, marginLeft: 12, marginTop: 12 },
});
