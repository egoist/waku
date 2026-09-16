import type { RuntimeMode } from '@waku/client';
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
import { StyleSheet, View } from 'react-native';

import { AppSymbol } from './app-symbol';
import type { ComposerAccessMenuProps } from './composer-access-menu.types';
import { Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { runtimeModeLabel } from '@/lib/session-presentation';

const ACCESS_MODES: Array<{
  id: RuntimeMode;
  description: string;
  icon: 'lock' | 'pencil' | 'sparkles' | 'lock.open';
}> = [
  { id: 'ask', description: 'Approve every command and file edit.', icon: 'lock' },
  {
    id: 'autoAcceptEdits',
    description: 'Edits apply automatically; commands still ask.',
    icon: 'pencil',
  },
  { id: 'auto', description: 'Works autonomously inside the project.', icon: 'sparkles' },
  {
    id: 'fullAccess',
    description: 'No approval prompts. The agent acts freely.',
    icon: 'lock.open',
  },
];

/** The task-page access control uses SwiftUI Menu so it stays attached to the
 * composer trigger and receives the native popover arrow and dismissal model. */
export function ComposerAccessMenu({ mode, onApply }: ComposerAccessMenuProps) {
  const theme = useTheme();
  const selected = ACCESS_MODES.find((item) => item.id === mode) ?? ACCESS_MODES[3]!;

  return (
    <Host ignoreSafeArea="all" matchContents>
      <Menu
        label={(
          <RNHostView matchContents>
            <View accessible={false} style={styles.trigger}>
              <AppSymbol name={selected.icon} size={19} tintColor={theme.textSecondary} />
            </View>
          </RNHostView>
        )}
        modifiers={[
          accessibilityLabel(`Agent access, ${runtimeModeLabel(mode)}`),
          accessibilityAddTraits(['isButton']),
        ]}>
        <Section title="Agent access">
          {ACCESS_MODES.map((item) => (
            <Toggle
              isOn={item.id === mode}
              key={item.id}
              onIsOnChange={() => {
                void Haptics.selectionAsync();
                onApply(item.id);
              }}>
              <Label systemImage={item.icon}>
                <VStack alignment="leading" spacing={1}>
                  <SwiftUIText>{runtimeModeLabel(item.id)}</SwiftUIText>
                  <SwiftUIText
                    modifiers={[
                      font({ textStyle: 'caption' }),
                      foregroundStyle({ type: 'hierarchical', style: 'secondary' }),
                      lineLimit(2),
                    ]}>
                    {item.description}
                  </SwiftUIText>
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
    height: 36,
    justifyContent: 'center',
    width: 36,
  },
});
