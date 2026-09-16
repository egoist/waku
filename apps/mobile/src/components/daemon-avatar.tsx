import { StyleSheet, Text, View } from 'react-native';

import { Radius } from '@/constants/theme';
import { profileInitials } from '@/lib/daemon-profile';
import { useTheme } from '@/hooks/use-theme';

export function DaemonAvatar({ name, size = 42 }: { name: string; size?: number }) {
  const theme = useTheme();
  return (
    <View
      accessibilityElementsHidden
      style={[
        styles.avatar,
        {
          width: size,
          height: size,
          borderRadius: Math.min(Radius.medium, size / 3.5),
          backgroundColor: theme.accentSoft,
        },
      ]}>
      <Text style={[styles.initials, { color: theme.accent, fontSize: Math.round(size * 0.34) }]}>
        {profileInitials(name)}
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  avatar: {
    alignItems: 'center',
    justifyContent: 'center',
  },
  initials: {
    fontWeight: '700',
    letterSpacing: 0.2,
  },
});
