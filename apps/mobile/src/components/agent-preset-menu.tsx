import { useState } from 'react';
import { StyleSheet, Text, View } from 'react-native';

import { AppSymbol } from './app-symbol';
import type { AgentPresetMenuProps } from './agent-preset-menu.types';
import { AgentPresetSheet } from './session-option-sheets';
import { AppPressable } from '@/components/app-pressable';

import { Radius } from '@/constants/theme';
import { useProviderModels } from '@/hooks/use-daemon-data';
import { useTheme } from '@/hooks/use-theme';
import { providerLabel } from '@/lib/session-presentation';

const PERSON_ICON = { ios: 'person', android: 'person', web: 'person' } as const;

/** Non-iOS fallback. iOS replaces this with a native anchored popover. */
export function AgentPresetMenu({ provider, agentPreset, onApply }: AgentPresetMenuProps) {
  const theme = useTheme();
  const [open, setOpen] = useState(false);
  const probe = useProviderModels(provider);
  const presets = probe.data?.agent_presets ?? [];
  const selected = presets.find((preset) => preset.id === agentPreset)
    ?? presets.find((preset) => preset.is_default)
    ?? presets[0];
  const label = selected?.name ?? `${providerLabel(provider)} agent`;

  return (
    <>
      <AppPressable
        accessibilityLabel={`Agent preset, ${label}`}
        accessibilityRole="button"
        onPress={() => setOpen(true)}
        style={({ pressed }) => [
          styles.trigger,
          {
            borderColor: theme.border,
            opacity: pressed ? 0.55 : 1,
          },
        ]}>
        <AppSymbol name={PERSON_ICON} size={15} tintColor={theme.textSecondary} />
        <Text style={[styles.label, { color: theme.textSecondary }]} numberOfLines={1}>
          {label}
        </Text>
      </AppPressable>
      <AgentPresetSheet
        agentPreset={agentPreset}
        onApply={(selection) => {
          onApply(selection);
          setOpen(false);
        }}
        onDismiss={() => setOpen(false)}
        provider={provider}
        visible={open}
      />
    </>
  );
}

const styles = StyleSheet.create({
  trigger: {
    alignItems: 'center',
    borderWidth: 1,
    borderRadius: Radius.pill,
    flexDirection: 'row',
    gap: 6,
    height: 34,
    maxWidth: 160,
    paddingHorizontal: 10,
  },
  label: {
    fontSize: 13,
    fontWeight: '500',
  },
});
