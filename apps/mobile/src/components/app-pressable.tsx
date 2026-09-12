import {
  Platform,
  Pressable,
  StyleSheet,
  View,
  type PressableStateCallbackType,
  type PressableProps,
  type StyleProp,
  type ViewStyle,
} from 'react-native';

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
 *
 * Android ripple rounding: the native ripple is an unmasked full-bounds
 * drawable, and a view never clips its own background, so a rounded pressable
 * draws a square ripple unless a rounded `overflow: 'hidden'` PARENT clips it
 * (ReactViewGroup rounds dispatchDraw). When a style carries a borderRadius
 * and the ripple isn't `borderless`, this component inserts that clipping
 * host itself, splitting the style so layout stays where the parent expects
 * it: placement keys (margins, absolute offsets) move to the host, sizing
 * keys are mirrored so the inner pressable still fills it.
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

/** Placement keys consumed by the clip host; duplicated there they would
 * apply twice (margins don't collapse) or position the pressable inside a
 * collapsed wrapper (absolute offsets). */
const HostOnlyKeys = [
  'position',
  'top',
  'left',
  'right',
  'bottom',
  'zIndex',
  'margin',
  'marginHorizontal',
  'marginVertical',
  'marginTop',
  'marginBottom',
  'marginLeft',
  'marginRight',
  'marginStart',
  'marginEnd',
  'aspectRatio',
] as const;

/** Sizing keys mirrored onto the clip host so it occupies the same slot the
 * pressable would have; the pressable keeps them and fills the host. */
const HostMirrorKeys = [
  'flex',
  'flexGrow',
  'flexShrink',
  'flexBasis',
  'alignSelf',
  'width',
  'height',
  'minWidth',
  'maxWidth',
  'minHeight',
  'maxHeight',
  'borderTopLeftRadius',
  'borderTopRightRadius',
  'borderBottomLeftRadius',
  'borderBottomRightRadius',
] as const;

/** The clip host around a rounded pressable, or undefined when the ripple
 * needs no rounding (no radius, borderless ripple, or off Android). */
function rippleHost(
  pressedStyle: StyleProp<ViewStyle>,
): { host: ViewStyle; inner: ViewStyle | undefined } | undefined {
  if (Platform.OS !== 'android' || pressedStyle == null) return undefined;
  const flat = StyleSheet.flatten(pressedStyle);
  const radius = flat?.borderRadius;
  if (radius == null || radius === 0) return undefined;
  const host: Record<string, unknown> = { borderRadius: radius, overflow: 'hidden' };
  for (const key of HostMirrorKeys) {
    if (flat[key as keyof ViewStyle] != null) host[key] = flat[key as keyof ViewStyle];
  }
  const inner: Record<string, unknown> = { ...flat };
  for (const key of HostOnlyKeys) delete inner[key];
  return { host: host as ViewStyle, inner: inner as ViewStyle };
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
  // Only the resolved Android style needs the host; iOS/web keep the style
  // function so `pressed` opacity keeps working.
  const host = borderless
    ? undefined
    : rippleHost(pressedStyle as StyleProp<ViewStyle>);
  if (!host) {
    return (
      <Pressable
        android_ripple={ripple}
        hitSlop={hitSlop}
        style={pressedStyle as PressableProps['style']}
        {...props}
      />
    );
  }
  const { children, ...rest } = props as PressableProps & { children?: React.ReactNode };
  return (
    <View style={host.host}>
      <Pressable
        android_ripple={ripple}
        hitSlop={hitSlop}
        style={host.inner}
        {...rest}>
        {children}
      </Pressable>
    </View>
  );
}
