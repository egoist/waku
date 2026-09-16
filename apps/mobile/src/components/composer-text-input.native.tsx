import PasteInput from '@mattermost/react-native-paste-input';
import { TextInput } from 'react-native';

import type { ComposerTextInputProps } from './composer-text-input.types';

export function ComposerTextInput({
  inputRef,
  onPasteError,
  onPasteFiles,
  ...props
}: ComposerTextInputProps) {
  if (!onPasteFiles) return <TextInput ref={inputRef} {...props} />;

  return (
    <PasteInput
      ref={inputRef}
      {...props}
      disableCopyPaste={false}
      onPaste={(error, files) => {
        if (error) {
          onPasteError?.(error);
          return;
        }
        if (files.length) {
          onPasteFiles(files.map((file) => ({
            uri: file.uri,
            name: file.fileName,
            mimeType: file.type,
            size: file.fileSize,
          })));
        }
      }}
    />
  );
}
