import * as Haptics from 'expo-haptics';
import {
  Host,
  Label,
  Menu,
  RNHostView,
  Section,
  Text as SwiftUIText,
  Toggle,
  VStack,
} from '@expo/ui/swift-ui';
import {
  accessibilityAddTraits,
  accessibilityLabel,
  font,
  foregroundStyle,
  lineLimit,
} from '@expo/ui/swift-ui/modifiers';
import { StyleSheet, Text, View } from 'react-native';

import { AppSymbol } from './app-symbol';
import type { AgentPresetMenuProps } from './agent-preset-menu.types';
import { Radius } from '@/constants/theme';
import { useProviderModels } from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import { providerLabel } from '@/lib/session-presentation';

const PERSON_ICON = { ios: 'person', android: 'person', web: 'person' } as const;

/** SwiftUI Menu so the agent picker stays attached to the composer trigger and
 * receives the native popover arrow and dismissal model, mirroring the desktop
 * agent chip. */
export function AgentPresetMenu({ provider, agentPreset, onApply }: AgentPresetMenuProps) {
  const theme = useTheme();
  const probe = useProviderModels(provider);
  const presets = probe.data?.agent_presets ?? [];
  const selected = presets.find((preset) => preset.id === agentPreset)
    ?? presets.find((preset) => preset.is_default)
    ?? presets[0];
  const label = selected?.name ?? `${providerLabel(provider)} agent`;

  return (
    <Host ignoreSafeArea="all" matchContents>
      <Menu
        label={(
          <RNHostView matchContents>
            <View accessible={false} style={[styles.trigger, { borderColor: theme.border }]}>
              <AppSymbol name={PERSON_ICON} size={15} tintColor={theme.textSecondary} />
              <Text style={[styles.label, { color: theme.textSecondary }]} numberOfLines={1}>
                {label}
              </Text>
            </View>
          </RNHostView>
        )}
        modifiers={[
          accessibilityLabel(`Agent preset, ${label}`),
          accessibilityAddTraits(['isButton']),
        ]}>
        <Section title={`${providerLabel(provider)} agents`}>
          {presets.map((preset) => (
            <Toggle
              isOn={agentPreset === preset.id || (!agentPreset && preset.id === selected?.id)}
              key={preset.id}
              onIsOnChange={() => {
                void Haptics.selectionAsync();
                onApply({ agentPreset: preset.id });
              }}>
              <Label systemImage={preset.is_default ? 'star' : 'person'}>
                <VStack alignment="leading" spacing={1}>
                  <SwiftUIText>{preset.name}</SwiftUIText>
                  {preset.description
                    ? (
                      <SwiftUIText
                        modifiers={[
                          font({ textStyle: 'caption' }),
                          foregroundStyle({ type: 'hierarchical', style: 'secondary' }),
                          lineLimit(2),
                        ]}>
                        {preset.description}
                      </SwiftUIText>
                    )
                    : null}
                </VStack>
              </Label>
            </Toggle>
          ))}
        </Section>
      </Menu>
    </Host>
  );
}

const styles = StyleSheet.create({
  trigger: {
    alignItems: 'center',
    borderRadius: Radius.pill,
    borderWidth: 1,
    flexDirection: 'row',
    gap: 6,
    height: 34,
    maxWidth: 160,
    paddingHorizontal: 10,
  },
  label: {
    color: '#666666',
    fontSize: 13,
    fontWeight: '500',
  },
});
