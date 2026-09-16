import { TextInput } from 'react-native';

import type { ComposerTextInputProps } from './composer-text-input.types';

export function ComposerTextInput({
  inputRef,
  onPasteError: _onPasteError,
  onPasteFiles: _onPasteFiles,
  ...props
}: ComposerTextInputProps) {
  return <TextInput ref={inputRef} {...props} />;
}
