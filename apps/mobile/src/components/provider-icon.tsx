import type { ProviderKind } from '@waku/client';
import { useMemo } from 'react';
import { View } from 'react-native';
import { SvgXml } from 'react-native-svg';

import { PROVIDER_ICON_XML } from './provider-icons';
import { useTheme } from '@/hooks/use-theme';

/** Marks with a real brand color; monochrome brands (OpenAI, Cursor, Grok,
 * OpenCode, Fx, Pi) tint with the theme text color, and Oh My Pi's SVG
 * carries its own gradient. */
const PROVIDER_BRAND_COLORS: Partial<Record<ProviderKind, string>> = {
  claude: '#d97757',
  deepSeek: '#4d6bfe',
  amp: '#f34e3f',
};

/** Brand color for a provider mark, when the brand has one. */
export function providerBrandColor(provider: ProviderKind): string | undefined {
  return PROVIDER_BRAND_COLORS[provider];
}

/** Brand mark for an agent provider. */
export function ProviderIcon({
  provider,
  size = 18,
  color,
}: {
  provider: ProviderKind;
  size?: number;
  color?: string;
}) {
  const theme = useTheme();
  const tint = color ?? PROVIDER_BRAND_COLORS[provider] ?? theme.text;
  const xml = useMemo(
    () => PROVIDER_ICON_XML[provider].replaceAll('fill="#000"', `fill="${tint}"`),
    [provider, tint],
  );
  return (
    <View accessibilityElementsHidden style={{ height: size, width: size }}>
      <SvgXml height={size} width={size} xml={xml} />
    </View>
  );
}
