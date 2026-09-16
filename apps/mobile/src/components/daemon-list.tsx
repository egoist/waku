import { ActivityIndicator, Pressable, StyleSheet, Text, View } from 'react-native';

import { AppSymbol } from '@/components/app-symbol';
import { ConnectionStatus, connectionPhaseLabel } from '@/components/connection-status';
import { DaemonAvatar } from '@/components/daemon-avatar';
import { NativeTint, Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { useDaemon } from '@/lib/daemon-context';
import { displayHost, type DaemonProfile } from '@/lib/daemon-profile';

export function DaemonList({
  onEdit,
  onSelect,
  selectingId = null,
}: {
  onEdit?: (profile: DaemonProfile) => void;
  onSelect: (profile: DaemonProfile) => void;
  selectingId?: string | null;
}) {
  const theme = useTheme();
  const daemon = useDaemon();
  return (
    <View
      style={[
        styles.group,
        { backgroundColor: theme.overlay },
      ]}>
      {daemon.profiles.map((profile, index) => {
        const active = profile.id === daemon.activeProfile?.id;
        const selecting = profile.id === selectingId;
        return (
          <View key={profile.id}>
            <View style={styles.row}>
              <Pressable
                accessibilityHint="Switches to this daemon"
                accessibilityLabel={`${profile.name}, ${displayHost(profile.address)}, ${
                  active ? connectionPhaseLabel(daemon.phase) : 'saved daemon'
                }`}
                accessibilityRole="button"
                accessibilityState={{ selected: active }}
                onPress={() => onSelect(profile)}
                style={({ pressed }) => [
                  styles.rowMain,
                  { backgroundColor: pressed ? theme.overlayStrong : 'transparent' },
                ]}>
                <DaemonAvatar name={profile.name} size={36} />
                <View style={styles.copy}>
                  <View style={styles.nameLine}>
                    <Text numberOfLines={1} style={[styles.name, { color: theme.text }]}>
                      {profile.name}
                    </Text>
                    {active && <ConnectionStatus compact phase={daemon.phase} />}
                  </View>
                  <Text numberOfLines={1} style={[styles.host, { color: theme.textSecondary }]}>
                    {displayHost(profile.address)}
                  </Text>
                </View>
                {selecting ? (
                  <ActivityIndicator color={NativeTint} size="small" />
                ) : active ? (
                  <AppSymbol
                    name={{ ios: 'checkmark', android: 'check', web: 'check' }}
                    size={16}
                    tintColor={NativeTint}
                  />
                ) : null}
              </Pressable>
              {onEdit && (
                <Pressable
                  accessibilityLabel={`Edit ${profile.name}`}
                  accessibilityRole="button"
                  hitSlop={6}
                  onPress={() => onEdit(profile)}
                  style={({ pressed }) => [styles.infoButton, { opacity: pressed ? 0.45 : 1 }]}>
                  <AppSymbol
                    name={{ ios: 'info.circle', android: 'info', web: 'info' }}
                    size={20}
                    tintColor={NativeTint}
                  />
                </Pressable>
              )}
            </View>
            {index < daemon.profiles.length - 1 && (
              <View style={[styles.separator, { backgroundColor: theme.separator }]} />
            )}
          </View>
        );
      })}
    </View>
  );
}

const styles = StyleSheet.create({
  group: {
    borderRadius: Radius.large,
    overflow: 'hidden',
  },
  row: { alignItems: 'stretch', flexDirection: 'row', minHeight: 64 },
  rowMain: {
    alignItems: 'center',
    flex: 1,
    flexDirection: 'row',
    gap: 12,
    minWidth: 0,
    paddingLeft: 12,
    paddingRight: 10,
    paddingVertical: 9,
  },
  copy: { flex: 1, minWidth: 0 },
  nameLine: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  name: { flexShrink: 1, fontSize: 16, fontWeight: '500' },
  host: { fontSize: 12.5, marginTop: 3 },
  infoButton: { alignItems: 'center', justifyContent: 'center', paddingHorizontal: 13 },
  separator: { height: StyleSheet.hairlineWidth, marginLeft: 60 },
});
