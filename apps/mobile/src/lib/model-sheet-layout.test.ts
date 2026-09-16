import { describe, expect, test } from 'bun:test';

import { modelSheetListHeight } from './model-sheet-layout';

const phone = {
  windowHeight: 844,
  keyboardTop: null,
  safeAreaTop: 59,
  safeAreaBottom: 34,
  fontScale: 1,
};

describe('model sheet keyboard layout', () => {
  test('keeps the original list height with the keyboard closed', () => {
    expect(modelSheetListHeight(phone)).toBe(405);
    expect(modelSheetListHeight({ ...phone, keyboardTop: 844 })).toBe(405);
  });

  test('leaves room above the keyboard for the search controls and safe areas', () => {
    const height = modelSheetListHeight({ ...phone, keyboardTop: 510 });
    expect(height).toBeLessThan(405);
    expect(height + 128 + phone.safeAreaBottom).toBeLessThanOrEqual(510 - phone.safeAreaTop);
  });

  test('does not subtract the keyboard twice when Android resizes the window', () => {
    expect(modelSheetListHeight({
      ...phone,
      windowHeight: 510,
      keyboardTop: 510,
    })).toBe(245);
  });

  test('reserves more room for larger text and never gives the list a negative height', () => {
    const height = modelSheetListHeight({ ...phone, keyboardTop: 510, fontScale: 2 });
    expect(height + 256 + phone.safeAreaBottom).toBeLessThanOrEqual(510 - phone.safeAreaTop);
    expect(modelSheetListHeight({
      ...phone,
      windowHeight: 390,
      keyboardTop: 160,
      fontScale: 2,
    })).toBe(0);
  });
});
