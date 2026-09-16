import { createContext, useContext } from 'react';

/**
 * Keep a row's top edge fixed on screen across a local layout change.
 *
 * In the inverted transcript a row's fixed native edge is its visual bottom,
 * so a disclosure that expands in place would grow *upward* and carry the
 * tapped header away. The list answers by pointing the native scroll anchor
 * (`maintainVisibleContentPosition.minIndexForVisible`) at the row's older
 * neighbour for exactly the commit that applies the change, which makes the
 * expansion grow downward the way every iOS disclosure does. `apply` runs one
 * commit later than the tap; the delay is a frame, the anchoring is atomic.
 */
export type KeepRowTop = (apply: () => void) => void;

const RowAnchorContext = createContext<KeepRowTop | null>(null);

export const RowAnchorProvider = RowAnchorContext.Provider;

/** Outside a transcript row the change simply applies immediately. */
export function useRowAnchor(): KeepRowTop {
  const keepTop = useContext(RowAnchorContext);
  return keepTop ?? applyNow;
}

function applyNow(apply: () => void) {
  apply();
}
