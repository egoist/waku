import type { TextInputProps } from 'react-native';
import type { RefCallback } from 'react';

import type { LocalAttachmentFile } from '@/lib/attachments';

export interface ComposerTextInputProps extends TextInputProps {
  inputRef?: RefCallback<ComposerTextInputHandle>;
  onPasteFiles?: (files: LocalAttachmentFile[]) => void;
  onPasteError?: (message: string) => void;
}

export interface ComposerTextInputHandle {
  focus: () => void;
}
