import type { TranscriptBlock } from '@waku/client';
import { activitiesForBlock } from '@waku/client/event-reducer';
import { activityHeaderTitle } from '@waku/client/transcript-presentation';
import { memo } from 'react';
import { Pressable, StyleSheet, Text } from 'react-native';

import { useActivitySheet } from './activity-sheet';
import { AppSymbol } from './app-symbol';
import { useTheme } from '@/hooks/use-theme';

/**
 * The transcript's view of a tool group is the desktop's collapsed summary
 * line — the live activity's title while streaming, "Ran N commands · …"
 * once settled — and nothing else. Tapping it opens the group in the native
 * activity sheet (`ActivitySheetHost`) rather than unfolding cards in
 * place: a phone transcript stays a readable feed, and the inverted,
 * anchored list never has to absorb an in-place layout change.
 */
export const ActivityGroup = memo(function ActivityGroup({
  block,
  blockIndex,
  live,
}: {
  block: TranscriptBlock;
  blockIndex: number;
  live: boolean;
}) {
  const theme = useTheme();
  const openSheet = useActivitySheet();
  const activities = activitiesForBlock(block);
  if (!activities.length) return null;
  return (
    <Pressable
      accessibilityHint="Opens the activity list"
      accessibilityRole="button"
      hitSlop={{ top: 6, bottom: 6 }}
      onPress={() => openSheet({
        blockIndex,
        turnId: block.turn_id,
        afterMessage: block.after_message,
      })}
      style={({ pressed }) => [styles.header, { opacity: pressed ? 0.6 : 1 }]}>
      <Text numberOfLines={1} style={[styles.title, { color: theme.textSecondary }]}>
        {activityHeaderTitle(activities, live)}
      </Text>
      <AppSymbol
        name={{ ios: 'chevron.right', android: 'chevron_right', web: 'chevron_right' }}
        size={10}
        tintColor={theme.textGhost}
      />
    </Pressable>
  );
});

const styles = StyleSheet.create({
  header: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 6,
    minHeight: 28,
  },
  title: { flexShrink: 1, fontSize: 13, fontWeight: '500' },
});
