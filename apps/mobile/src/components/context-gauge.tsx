import { StyleSheet, Text, View } from 'react-native';

import { Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';

/** Compact context-window meter for the composer chip row. */
export function ContextGauge({ percent }: { percent: number }) {
  const theme = useTheme();
  const color = percent >= 95 ? theme.danger : percent >= 80 ? theme.warning : theme.textTertiary;
  return (
    <View
      accessibilityLabel={`Context ${percent} percent full`}
      style={styles.container}>
      <View style={[styles.track, { backgroundColor: theme.overlayStrong }]}>
        <View
          style={[
            styles.fill,
            { backgroundColor: color, width: `${Math.max(4, Math.min(100, percent))}%` },
          ]}
        />
      </View>
      <Text style={[styles.label, { color }]}>{percent}%</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  container: { alignItems: 'center', flexDirection: 'row', gap: 5 },
  track: { borderRadius: Radius.pill, height: 4, overflow: 'hidden', width: 30 },
  fill: { borderRadius: Radius.pill, height: 4 },
  label: { fontSize: 10.5, fontVariant: ['tabular-nums'], fontWeight: '600' },
});
