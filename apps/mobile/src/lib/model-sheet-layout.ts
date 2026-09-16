/** Reserve the native handle, title/back row, search field, and spacing.
 * These stay visible while the model list gives up space to the keyboard. */
const MODEL_SHEET_CONTROLS_HEIGHT = 128;

export function modelSheetListHeight({
  windowHeight,
  keyboardTop,
  safeAreaTop,
  safeAreaBottom,
  fontScale,
}: {
  windowHeight: number;
  keyboardTop: number | null;
  safeAreaTop: number;
  safeAreaBottom: number;
  fontScale: number;
}): number {
  // Android can resize the window for the keyboard already. Cap at its top
  // edge instead of subtracting its height from that resized window again.
  const availableHeight = Math.min(windowHeight, keyboardTop ?? windowHeight);
  const controlsHeight = Math.ceil(MODEL_SHEET_CONTROLS_HEIGHT * Math.max(1, fontScale));
  const bottomPadding = Math.max(safeAreaBottom, 14);
  return Math.max(0, Math.min(
    Math.round(windowHeight * 0.48),
    Math.floor(availableHeight - safeAreaTop - bottomPadding - controlsHeight),
  ));
}
