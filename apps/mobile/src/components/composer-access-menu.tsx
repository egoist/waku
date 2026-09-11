import { useState } from 'react';
import { StyleSheet } from 'react-native';

import { AppSymbol } from './app-symbol';
import type { ComposerAccessMenuProps } from './composer-access-menu.types';
import { AccessSheet } from './session-option-sheets';
import { AppPressable } from '@/components/app-pressable';

import { Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { runtimeModeLabel } from '@/lib/session-presentation';

const MODE_ICONS = {
  ask: { ios: 'lock', android: 'lock', web: 'lock' },
  autoAcceptEdits: { ios: 'pencil', android: 'edit', web: 'edit' },
  auto: { ios: 'sparkles', android: 'auto_awesome', web: 'auto_awesome' },
  fullAccess: { ios: 'lock.open', android: 'lock_open', web: 'lock_open' },
} as const;

/** Non-iOS fallback. iOS replaces this with a native anchored menu. */
export function ComposerAccessMenu({ mode, onApply }: ComposerAccessMenuProps) {
  const theme = useTheme();
  const [open, setOpen] = useState(false);

  return (
    <>
      <AppPressable
        accessibilityLabel={`Agent access, ${runtimeModeLabel(mode)}`}
        accessibilityRole="button"
        onPress={() => setOpen(true)}
        style={({ pressed }) => [styles.trigger, { opacity: pressed ? 0.55 : 1 }]}>
        <AppSymbol name={MODE_ICONS[mode]} size={19} tintColor={theme.textSecondary} />
      </AppPressable>
      <AccessSheet
        mode={mode}
        onApply={onApply}
        onDismiss={() => setOpen(false)}
        visible={open}
      />
    </>
  );
}

const styles = StyleSheet.create({
  trigger: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    height: 36,
    justifyContent: 'center',
    width: 36,
  },
});
