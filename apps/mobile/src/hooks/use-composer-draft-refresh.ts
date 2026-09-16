import { useFocusEffect } from 'expo-router';
import { useCallback, useEffect, useState } from 'react';
import { AppState } from 'react-native';

/**
 * Changes whenever a mounted composer becomes visible again. Route focus and
 * app foregrounding are separate on native navigation, so both boundaries
 * participate in cross-device draft refreshes.
 */
export function useComposerDraftRefreshRevision(): number | null {
  const [revision, setRevision] = useState<number | null>(null);
  const refresh = useCallback(() => {
    setRevision((value) => value === null ? 0 : value + 1);
  }, []);

  useFocusEffect(refresh);

  useEffect(() => {
    let previous = AppState.currentState;
    const subscription = AppState.addEventListener('change', (state) => {
      if (state === 'active' && previous !== 'active') {
        refresh();
      }
      previous = state;
    });
    return () => subscription.remove();
  }, [refresh]);

  return revision;
}
