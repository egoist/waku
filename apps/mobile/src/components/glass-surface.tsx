import { GlassView, isLiquidGlassAvailable } from 'expo-glass-effect';
import type { ReactNode } from 'react';
import { Platform, StyleSheet, View, type StyleProp, type ViewStyle } from 'react-native';

import { useTheme } from '@/hooks/use-theme';

/** True when the OS renders real Liquid Glass (iOS 26+). */
export const liquidGlass = Platform.OS === 'ios' && isLiquidGlassAvailable();

/**
 * Floating chrome surface: Liquid Glass where the OS supports it, a subtle
 * overlay fill elsewhere. The system material draws its own rim and
 * elevation — no hand-rolled shadows. Don't wrap it in overflow:'hidden';
 * the glass masks to borderRadius natively.
 */
export function GlassSurface({
  style,
  children,
  interactive = false,
  fallbackColor,
}: {
  style?: StyleProp<ViewStyle>;
  children: ReactNode;
  interactive?: boolean;
  fallbackColor?: string;
}) {
  const theme = useTheme();
  if (liquidGlass) {
    return (
      <GlassView glassEffectStyle="regular" isInteractive={interactive} style={style}>
        {children}
      </GlassView>
    );
  }
  return (
    <View style={[{ backgroundColor: fallbackColor ?? theme.overlayStrong }, style]}>
      {children}
    </View>
  );
}
