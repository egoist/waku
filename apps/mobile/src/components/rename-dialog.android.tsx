import {
  AlertDialog,
  Host,
  OutlinedTextField,
  Text,
  TextButton,
  useNativeState,
} from '@expo/ui/jetpack-compose';
import { fillMaxWidth } from '@expo/ui/jetpack-compose/modifiers';
import { useRef, useState } from 'react';
import { StyleSheet } from 'react-native';

import type { RenameDialogProps } from '@/components/rename-dialog.types';

export function RenameDialog(props: RenameDialogProps) {
  return props.visible ? <NativeRenameDialog {...props} /> : null;
}

function NativeRenameDialog({ initialValue, onDismiss, onSubmit }: RenameDialogProps) {
  const title = useNativeState(initialValue);
  const selection = useNativeState({ start: 0, end: initialValue.length });
  const saving = useRef(false);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function save(value: string) {
    if (saving.current) return;
    saving.current = true;
    setIsSaving(true);
    setError(null);
    void onSubmit(value)
      .then(onDismiss)
      .catch((cause) => {
        saving.current = false;
        setIsSaving(false);
        setError(cause instanceof Error ? cause.message : String(cause));
      });
  }

  return (
    <Host matchContents pointerEvents="box-none" style={styles.host}>
      <AlertDialog
        onDismissRequest={() => {
          if (!saving.current) onDismiss();
        }}>
        <AlertDialog.Title>
          <Text>Rename task</Text>
        </AlertDialog.Title>
        <AlertDialog.Text>
          <OutlinedTextField
            autoFocus
            enabled={!isSaving}
            isError={Boolean(error)}
            keyboardActions={{ onDone: save }}
            keyboardOptions={{ imeAction: 'done' }}
            modifiers={[fillMaxWidth()]}
            selection={selection}
            singleLine
            value={title}>
            <OutlinedTextField.Label>
              <Text>Task title</Text>
            </OutlinedTextField.Label>
            {error ? (
              <OutlinedTextField.SupportingText>
                <Text>{error}</Text>
              </OutlinedTextField.SupportingText>
            ) : null}
          </OutlinedTextField>
        </AlertDialog.Text>
        <AlertDialog.DismissButton>
          <TextButton enabled={!isSaving} onClick={onDismiss}>
            <Text>Cancel</Text>
          </TextButton>
        </AlertDialog.DismissButton>
        <AlertDialog.ConfirmButton>
          <TextButton enabled={!isSaving} onClick={() => save(title.get())}>
            <Text>Rename</Text>
          </TextButton>
        </AlertDialog.ConfirmButton>
      </AlertDialog>
    </Host>
  );
}

const styles = StyleSheet.create({
  host: { height: 1, left: 0, position: 'absolute', top: 0, width: 1 },
});
