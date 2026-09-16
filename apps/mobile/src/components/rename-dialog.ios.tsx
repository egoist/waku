import {
  Alert as SwiftUIAlert,
  Button,
  Host,
  Text,
  TextField,
  useNativeState,
} from '@expo/ui/swift-ui';
import { onSubmit as onNativeSubmit, opacity, submitLabel } from '@expo/ui/swift-ui/modifiers';
import { useRef } from 'react';
import { Alert, StyleSheet } from 'react-native';

import type { RenameDialogProps } from '@/components/rename-dialog.types';

export function RenameDialog(props: RenameDialogProps) {
  return props.visible ? <NativeRenameDialog {...props} /> : null;
}

function NativeRenameDialog({ initialValue, onDismiss, onSubmit }: RenameDialogProps) {
  const title = useNativeState(initialValue);
  const saving = useRef(false);

  function save() {
    if (saving.current) return;
    saving.current = true;
    void onSubmit(title.get())
      .then(onDismiss)
      .catch((cause) => {
        onDismiss();
        Alert.alert(
          'Couldn\u2019t rename task',
          cause instanceof Error ? cause.message : String(cause),
        );
      });
  }

  return (
    <Host pointerEvents="box-none" style={styles.host}>
      <SwiftUIAlert
        isPresented
        onIsPresentedChange={(isPresented) => {
          if (!isPresented) onDismiss();
        }}
        title="Rename task">
        <SwiftUIAlert.Trigger>
          <Text modifiers={[opacity(0)]}>{'\u200B'}</Text>
        </SwiftUIAlert.Trigger>
        <SwiftUIAlert.Actions>
          <TextField
            autoFocus
            modifiers={[submitLabel('done'), onNativeSubmit(save)]}
            placeholder="Task title"
            text={title}
          />
          <Button label="Cancel" role="cancel" />
          <Button label="Rename" onPress={save} />
        </SwiftUIAlert.Actions>
      </SwiftUIAlert>
    </Host>
  );
}

const styles = StyleSheet.create({
  host: { height: 1, left: 0, position: 'absolute', top: 0, width: 1 },
});
