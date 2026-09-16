import { Button, Host, Menu, RNHostView } from '@expo/ui/swift-ui';
import {
  accessibilityAddTraits,
  accessibilityHint,
  accessibilityLabel,
} from '@expo/ui/swift-ui/modifiers';

import type { TaskRowMenuProps } from '@/components/task-row-menu.types';

/**
 * SwiftUI Menu's primary action fires a normal tap directly while reserving
 * long-press for the native menu. ContextMenu delays its hosted React Native
 * child's tap while it arbitrates the long-press gesture.
 */
export function TaskRowMenu({
  accessibilityLabel: label,
  onDelete,
  onRename,
  onSelect,
  renderTrigger,
  selected,
  style,
}: TaskRowMenuProps) {
  return (
    <Host ignoreSafeArea="all" matchContents style={style}>
      <Menu
        label={<RNHostView matchContents>{renderTrigger(false)}</RNHostView>}
        modifiers={[
          accessibilityLabel(label),
          accessibilityHint('Long press for actions'),
          accessibilityAddTraits(selected ? ['isButton', 'isSelected'] : ['isButton']),
        ]}
        onPrimaryAction={onSelect}>
        <Button label="Rename task" systemImage="pencil" onPress={onRename} />
        <Button
          label="Delete task"
          role="destructive"
          systemImage="trash"
          onPress={onDelete}
        />
      </Menu>
    </Host>
  );
}
