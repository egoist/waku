import { useEffect, useState } from 'react';
import { AccessibilityInfo } from 'react-native';

let lastKnown = false;

/** Honors the system reduce-motion setting; decorative motion (the streaming
 * veil, pulses) must check this and skip, mirroring the desktop's
 * `cx.reduce_motion()` contract. */
export function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(lastKnown);
  useEffect(() => {
    let mounted = true;
    const update = (value: boolean) => {
      lastKnown = value;
      if (mounted) setReduced(value);
    };
    void AccessibilityInfo.isReduceMotionEnabled().then(update).catch(() => {});
    const subscription = AccessibilityInfo.addEventListener('reduceMotionChanged', update);
    return () => {
      mounted = false;
      subscription.remove();
    };
  }, []);
  return reduced;
}
