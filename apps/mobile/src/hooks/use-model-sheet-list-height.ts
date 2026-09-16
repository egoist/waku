import { useEffect, useState } from 'react';
import { Keyboard, Platform, useWindowDimensions, type KeyboardEvent } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { modelSheetListHeight } from '@/lib/model-sheet-layout';

/** Fitted native sheets retain their content's measured height when the
 * keyboard opens. Shrink the list so the search controls stay in the safe area. */
export function useModelSheetListHeight(visible: boolean): number {
  const { height, fontScale } = useWindowDimensions();
  const insets = useSafeAreaInsets();
  const [keyboardTop, setKeyboardTop] = useState<number | null>(
    () => Keyboard.metrics?.()?.screenY ?? null,
  );

  useEffect(() => {
    if (!visible) return;
    const updateFrame = (event: KeyboardEvent) => {
      setKeyboardTop(event.endCoordinates.height > 0 ? event.endCoordinates.screenY : null);
    };
    const frame = Keyboard.addListener(
      Platform.OS === 'ios' ? 'keyboardWillChangeFrame' : 'keyboardDidShow',
      updateFrame,
    );
    const hide = Keyboard.addListener(
      Platform.OS === 'ios' ? 'keyboardWillHide' : 'keyboardDidHide',
      () => setKeyboardTop(null),
    );
    setKeyboardTop(Keyboard.metrics?.()?.screenY ?? null);
    return () => {
      frame.remove();
      hide.remove();
    };
  }, [visible]);

  return modelSheetListHeight({
    windowHeight: height,
    keyboardTop,
    safeAreaTop: insets.top,
    safeAreaBottom: insets.bottom,
    fontScale,
  });
}
