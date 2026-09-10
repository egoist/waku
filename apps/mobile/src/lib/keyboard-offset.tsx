import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { Keyboard, Platform } from "react-native";

const KeyboardHeightContext = createContext(0);

/**
 * Tracks the visible software-keyboard height once, app-wide. Edge-to-edge
 * Android delivers the keyboard as a WindowInset rather than resizing the
 * window, so components that must clear it (the composer) read this instead of
 * each installing their own Keyboard listener. Returns 0 on iOS, where
 * KeyboardAvoidingView already lifts content, so every consumer can add the
 * value unconditionally without double-lifting.
 */
export function KeyboardOffsetProvider({ children }: { children: ReactNode }) {
  const [rawHeight, setRawHeight] = useState(0);
  useEffect(() => {
    const show = Keyboard.addListener("keyboardDidShow", (event) => {
      setRawHeight(event.endCoordinates.height);
    });
    const hide = Keyboard.addListener("keyboardDidHide", () => setRawHeight(0));
    return () => {
      show.remove();
      hide.remove();
    };
  }, []);
  const height = Platform.OS === "android" ? rawHeight : 0;
  return (
    <KeyboardHeightContext.Provider value={height}>
      {children}
    </KeyboardHeightContext.Provider>
  );
}

export function useKeyboardHeight(): number {
  return useContext(KeyboardHeightContext);
}
