import { MenuView, type MenuAction } from '@expo/ui/community/menu';
import { Pressable } from 'react-native';

import type { TaskRowMenuProps } from '@/components/task-row-menu.types';

const Actions: MenuAction[] = [
  { id: 'rename', title: 'Rename task', image: 'pencil' },
  {
    id: 'delete',
    title: 'Delete task',
    image: 'trash',
    attributes: { destructive: true },
  },
];

export function TaskRowMenu({
  accessibilityLabel,
  onDelete,
  onRename,
  onSelect,
  renderTrigger,
  selected,
  style,
}: TaskRowMenuProps) {
  return (
    <MenuView
      actions={Actions}
      onPressAction={({ nativeEvent }) => {
        if (nativeEvent.event === 'rename') onRename();
        else if (nativeEvent.event === 'delete') onDelete();
      }}
      shouldOpenOnLongPress
      style={style}>
      <Pressable
        accessibilityHint="Long press for actions"
        accessibilityLabel={accessibilityLabel}
        accessibilityRole="button"
        accessibilityState={{ selected }}
        onPress={onSelect}>
        {({ pressed }) => renderTrigger(pressed)}
      </Pressable>
    </MenuView>
  );
}
