import { Platform, Pressable, type PressableStateCallbackType, type PressableProps, type StyleProp, type ViewStyle } from 'react-native';

import { Colors, StateLayer } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';

/**
 * Every pressable in the app. Android gets a real Material ripple instead of
 * the opacity fade a cross-platform default gives you, and every target picks
 * up Material's minimum touch padding — <Pressable> alone gives neither, and
 * hand-rolling it at sixty call sites let both drift.
 *
 * Pressed feedback belongs here, not downstream: an `opacity: pressed ? …`
 * in a style callback dims the label along with the surface, which reads as a
 * flicker rather than a touch, and fights the ripple on Android.
 */
type AppPressableStyle =
  | StyleProp<ViewStyle>
  | ((state: PressableStateCallbackType) => StyleProp<ViewStyle>);

export interface AppPressableProps extends Omit<PressableProps, 'style'> {
  /** Reveal past the view's bounds — the circular ripple pill and round
   * buttons want, matching how Android draws borderless icon buttons. */
  borderless?: boolean;
  style?: AppPressableStyle;
}

export function AppPressable({
  android_ripple,
  borderless = false,
  hitSlop = 8,
  style,
  ...props
}: AppPressableProps) {
  const theme = useTheme();
  const ripple = android_ripple ?? {
    borderless,
    color: theme === Colors.dark ? StateLayer.dark : StateLayer.light,
  };
  // Android already reports the touch through the ripple, so a downstream
  // `pressed ? 0.6 : 1` just dims the label on top of it and reads as a
  // flicker. iOS and web have no such affordance and keep the fade.
  const pressedStyle = Platform.OS === 'android'
    ? (typeof style === 'function' ? style({ pressed: false, hovered: false }) : style)
    : style;
  return (
    <Pressable
      android_ripple={ripple}
      hitSlop={hitSlop}
      style={pressedStyle as PressableProps['style']}
      {...props}
    />
  );
}
