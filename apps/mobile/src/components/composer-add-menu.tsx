import { MenuView, type MenuAction } from '@expo/ui/community/menu';
import { StyleSheet, View } from 'react-native';

import { AppSymbol } from './app-symbol';
import { Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';

export type ComposerAttachmentSource = 'files' | 'camera' | 'photo';

const AttachmentActions: MenuAction[] = [
  { id: 'files', title: 'Files', image: 'folder' },
  { id: 'camera', title: 'Camera', image: 'camera' },
  { id: 'photo', title: 'Photo', image: 'photo' },
];

export function ComposerAddMenu({
  disabled = false,
  onChoose,
  onChooseContext,
}: {
  disabled?: boolean;
  onChoose?: (source: ComposerAttachmentSource) => void;
  onChooseContext: (kind: 'command' | 'file') => void;
}) {
  const theme = useTheme();
  const trigger = (
    <View
      accessible
      accessibilityLabel="Add to message"
      accessibilityRole="button"
      accessibilityState={{ disabled }}
      style={[styles.trigger, { opacity: disabled ? 0.35 : 1 }]}>
      <AppSymbol
        name={{ ios: 'plus', android: 'add', web: 'add' }}
        size={20}
        tintColor={theme.textSecondary}
      />
    </View>
  );
  if (disabled) return trigger;

  return (
    <MenuView
      actions={[
        { id: 'command', title: 'Commands', image: 'command' },
        { id: 'mention', title: 'Mention files', image: 'at' },
        ...(onChoose ? AttachmentActions : []),
      ]}
      onPressAction={({ nativeEvent }) => {
        const source = nativeEvent.event;
        // Let the native menu finish dismissing before presenting a picker.
        setTimeout(() => {
          if (source === 'command' || source === 'mention') onChooseContext(source === 'command' ? 'command' : 'file');
          else if (source === 'files' || source === 'camera' || source === 'photo') onChoose?.(source);
        }, 160);
      }}>
      {trigger}
    </MenuView>
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
