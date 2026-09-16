import type { ReactElement } from 'react';
import type { StyleProp, ViewStyle } from 'react-native';

export interface TaskRowMenuProps {
  accessibilityLabel: string;
  onDelete: () => void;
  onRename: () => void;
  onSelect: () => void;
  renderTrigger: (pressed: boolean) => ReactElement;
  selected: boolean;
  style: StyleProp<ViewStyle>;
}
