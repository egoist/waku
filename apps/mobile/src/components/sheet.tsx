import {
  BottomSheetModal,
  BottomSheetScrollView,
  BottomSheetView,
  type BottomSheetMethods,
} from '@expo/ui/community/bottom-sheet';
import { useEffect, useRef, type ReactNode } from 'react';
import { Pressable, StyleSheet, Text, View, useWindowDimensions } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { AppSymbol } from '@/components/app-symbol';
import { liquidGlass } from '@/components/glass-surface';
import { NativeTint, Radius, Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';

/**
 * Bottom sheet used for pickers and action menus, presented through the
 * platform's native sheet (SwiftUI detents on iOS, Material 3 on Android)
 * via @expo/ui. The declarative `visible` prop drives the imperative
 * present/dismiss API so call sites stay simple; swipe-down, backdrop tap,
 * and the Android back gesture all dismiss.
 */
export function Sheet({
  visible,
  onDismiss,
  title,
  children,
  scrollable = true,
}: {
  visible: boolean;
  onDismiss: () => void;
  title?: string;
  children: ReactNode;
  scrollable?: boolean;
}) {
  const theme = useTheme();
  const insets = useSafeAreaInsets();
  const { height } = useWindowDimensions();
  const sheet = useRef<BottomSheetMethods>(null);

  useEffect(() => {
    if (visible) sheet.current?.present();
    else sheet.current?.dismiss();
  }, [visible]);

  const body = (
    <>
      {title ? (
        <Text style={[styles.title, { color: theme.textSecondary }]}>{title}</Text>
      ) : null}
      {children}
    </>
  );
  return (
    <BottomSheetModal
      ref={sheet}
      backgroundStyle={liquidGlass ? undefined : { backgroundColor: theme.surface }}
      enablePanDownToClose
      onDismiss={onDismiss}>
      <BottomSheetView
        style={[styles.content, { paddingBottom: Math.max(insets.bottom, 14) }]}>
        {scrollable ? (
          <BottomSheetScrollView
            alwaysBounceVertical={false}
            keyboardShouldPersistTaps="handled"
            showsVerticalScrollIndicator={false}
            style={{ maxHeight: Math.round(height * 0.62) }}>
            {body}
          </BottomSheetScrollView>
        ) : (
          body
        )}
      </BottomSheetView>
    </BottomSheetModal>
  );
}

export function SheetRow({
  label,
  description,
  selected = false,
  destructive = false,
  disabled = false,
  leading,
  onPress,
}: {
  label: string;
  description?: string | null;
  selected?: boolean;
  destructive?: boolean;
  disabled?: boolean;
  leading?: ReactNode;
  onPress: () => void;
}) {
  const theme = useTheme();
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ selected, disabled }}
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.row,
        {
          backgroundColor: pressed ? theme.overlayStrong : selected ? theme.overlay : 'transparent',
          opacity: disabled ? 0.4 : 1,
        },
      ]}>
      {leading}
      <View style={styles.rowCopy}>
        <Text
          numberOfLines={1}
          style={[styles.rowLabel, { color: destructive ? theme.danger : theme.text }]}>
          {label}
        </Text>
        {description ? (
          <Text numberOfLines={2} style={[styles.rowDescription, { color: theme.textTertiary }]}>
            {description}
          </Text>
        ) : null}
      </View>
      {selected && (
        <AppSymbol
          name={{ ios: 'checkmark', android: 'check', web: 'check' }}
          size={15}
          tintColor={NativeTint}
        />
      )}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  content: {
    paddingHorizontal: Spacing.two,
    paddingTop: 4,
  },
  title: {
    fontSize: 12,
    fontWeight: '600',
    letterSpacing: 0.4,
    marginBottom: 6,
    marginHorizontal: 12,
    marginTop: 6,
    textTransform: 'uppercase',
  },
  row: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    flexDirection: 'row',
    gap: 11,
    minHeight: 50,
    paddingHorizontal: 12,
    paddingVertical: 9,
  },
  rowCopy: { flex: 1, minWidth: 0 },
  rowLabel: { fontSize: 15.5, fontWeight: '500' },
  rowDescription: { fontSize: 12.5, lineHeight: 17, marginTop: 2 },
});
