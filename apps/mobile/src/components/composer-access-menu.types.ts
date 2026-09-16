import type { RuntimeMode } from '@waku/client';

export interface ComposerAccessMenuProps {
  mode: RuntimeMode;
  onApply: (mode: RuntimeMode) => void;
}
