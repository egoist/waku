import { ActivityIndicator, StyleSheet, Text, View } from 'react-native';

import { useTheme } from '@/hooks/use-theme';
import type { ConnectionPhase } from '@/lib/daemon-context';

export function ConnectionStatus({
  phase,
  compact = false,
}: {
  phase: ConnectionPhase;
  compact?: boolean;
}) {
  const theme = useTheme();
  const presentation = statusPresentation(phase, theme);
  return (
    <View style={styles.container}>
      {presentation.busy ? (
        <ActivityIndicator color={presentation.color} size="small" style={styles.spinner} />
      ) : (
        <View style={[styles.dot, { backgroundColor: presentation.color }]} />
      )}
      {!compact && (
        <Text style={[styles.label, { color: presentation.color }]}>{presentation.label}</Text>
      )}
    </View>
  );
}

/** The phase as a short word, for labels and accessibility. */
export function connectionPhaseLabel(phase: ConnectionPhase): string {
  switch (phase) {
    case 'connected':
      return 'Connected';
    case 'connecting':
    case 'booting':
      return 'Connecting';
    case 'reconnecting':
      return 'Reconnecting';
    case 'error':
      return 'Needs attention';
    default:
      return 'Offline';
  }
}

function statusPresentation(
  phase: ConnectionPhase,
  theme: ReturnType<typeof useTheme>,
): { label: string; color: string; busy: boolean } {
  const label = connectionPhaseLabel(phase);
  switch (phase) {
    case 'connected':
      return { label, color: theme.success, busy: false };
    case 'connecting':
    case 'booting':
    case 'reconnecting':
      return { label, color: theme.warning, busy: true };
    case 'error':
      return { label, color: theme.danger, busy: false };
    default:
      return { label, color: theme.textTertiary, busy: false };
  }
}

const styles = StyleSheet.create({
  container: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 6,
  },
  dot: {
    borderRadius: 999,
    height: 8,
    width: 8,
  },
  spinner: {
    height: 12,
    transform: [{ scale: 0.66 }],
    width: 12,
  },
  label: {
    fontSize: 12,
    fontWeight: '600',
  },
});
