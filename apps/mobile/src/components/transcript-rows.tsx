import type { AgentSession, Checkpoint, Message } from '@waku/client';
import { formatMessageTime } from '@waku/client/transcript-presentation';
import * as Clipboard from 'expo-clipboard';
import * as Haptics from 'expo-haptics';
import { memo, useEffect, useState } from 'react';
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from 'react-native';

import { ActivityGroup } from '@/components/activity-group';
import { AppSymbol } from '@/components/app-symbol';
import { AttachmentTile } from '@/components/attachment-tile';
import {
  MdBlockLive,
  MdBlockSettled,
  MdRevealOnMount,
  type VeilRegistry,
} from '@/components/md-block-row';
import { useRowAnchor } from '@/components/transcript-anchor';
import { MonoFont, Radius } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import type { TranscriptRow } from '@/lib/session-presentation';
import type { MarkdownStyles } from '@/md/render';
import type { TranscriptMarkdownCache } from '@/md/transcript-cache';

/**
 * One transcript row's content. Memoized on its props: the list hands it the
 * same row object whenever nothing about the row changed, so a stream commit
 * re-renders exactly the live tail block.
 */
export const TranscriptRowView = memo(function TranscriptRowView({
  row,
  md,
  veils,
  seeded,
  markdownStyles,
  onToggleFold,
}: {
  row: TranscriptRow;
  md: TranscriptMarkdownCache;
  veils: VeilRegistry;
  seeded: boolean;
  markdownStyles: MarkdownStyles;
  onToggleFold: (turnId: string) => void;
}) {
  const theme = useTheme();
  switch (row.kind) {
    case 'user':
      return <UserBubble message={row.message} />;
    case 'system':
      return (
        <View style={styles.systemFrame}>
          <Text style={[styles.systemMessage, { backgroundColor: theme.overlay, color: theme.textTertiary }]}>
            {row.message.display_content ?? row.message.content}
          </Text>
        </View>
      );
    case 'md':
      return (
        <>
          {row.live ? (
            <MdBlockLive
              md={md}
              messageId={row.messageId}
              node={row.node}
              rowKey={row.key}
              seeded={seeded}
              source={row.source}
              styles={markdownStyles}
              veils={veils}
            />
          ) : (
            <MdRevealOnMount enabled={row.streaming && !seeded}>
              <MdBlockSettled node={row.node} source={row.source} styles={markdownStyles} />
            </MdRevealOnMount>
          )}
          {row.footerTimestamp != null && (
            <Text style={[styles.messageFooter, { color: theme.textGhost }]}>
              {formatMessageTime(row.footerTimestamp)}
            </Text>
          )}
        </>
      );
    case 'activities':
      return <ActivityGroup block={row.block} blockIndex={row.blockIndex} live={row.live} />;
    case 'fold':
      return (
        <FoldRow
          expanded={row.expanded}
          label={row.label}
          turnId={row.turn.id}
          onToggle={onToggleFold}
        />
      );
    case 'changed':
      return <ChangedFilesCard checkpoint={row.checkpoint} />;
  }
});

/** Desktop's turn fold: a hairline divider carrying "Worked for X ›". */
function FoldRow({
  label,
  expanded,
  turnId,
  onToggle,
}: {
  label: string;
  expanded: boolean;
  turnId: string;
  onToggle: (turnId: string) => void;
}) {
  const theme = useTheme();
  const keepTop = useRowAnchor();
  return (
    <Pressable
      accessibilityHint={expanded ? 'Collapses the agent’s work' : 'Shows the agent’s work'}
      accessibilityLabel={label}
      accessibilityRole="button"
      accessibilityState={{ expanded }}
      hitSlop={{ top: 6, bottom: 6 }}
      onPress={() => keepTop(() => onToggle(turnId))}
      style={({ pressed }) => [styles.foldRow, { opacity: pressed ? 0.6 : 1 }]}>
      <View style={[styles.foldLine, { backgroundColor: theme.border }]} />
      <Text numberOfLines={1} style={[styles.foldLabel, { color: theme.textTertiary }]}>
        {label}
      </Text>
      <AppSymbol
        name={expanded
          ? { ios: 'chevron.down', android: 'keyboard_arrow_down', web: 'keyboard_arrow_down' }
          : { ios: 'chevron.right', android: 'chevron_right', web: 'chevron_right' }}
        size={10}
        tintColor={theme.textGhost}
      />
      <View style={[styles.foldLine, { backgroundColor: theme.border }]} />
    </Pressable>
  );
}

const UserBubble = memo(
  UserBubbleInner,
  (previous, next) =>
    previous.message.id === next.message.id &&
    previous.message.content === next.message.content &&
    previous.message.display_content === next.message.display_content &&
    previous.message.attachments === next.message.attachments,
);

function UserBubbleInner({ message }: { message: Message }) {
  const theme = useTheme();
  const content = message.display_content ?? message.content;

  async function copy() {
    await Clipboard.setStringAsync(message.content);
    await Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
  }

  return (
    <View style={styles.userFrame}>
      {message.attachments?.length ? (
        <View style={styles.attachments}>
          {message.attachments.map((attachment, index) => (
            <AttachmentTile
              attachment={attachment}
              key={`${attachment.blob_reference ?? attachment.path}:${index}`}
            />
          ))}
        </View>
      ) : null}
      {content ? (
        <Pressable
          accessibilityHint="Long press to copy"
          delayLongPress={350}
          onLongPress={() => void copy()}
          style={[styles.userBubble, { backgroundColor: theme.raised }]}>
          <Text selectable style={[styles.userText, { color: theme.text }]}>{content}</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

function ChangedFilesCard({ checkpoint }: { checkpoint: Checkpoint }) {
  const theme = useTheme();
  const keepTop = useRowAnchor();
  const [open, setOpen] = useState(false);
  return (
    <View style={[styles.changedCard, { backgroundColor: theme.surface, borderColor: theme.border }]}>
      <Pressable
        accessibilityRole="button"
        accessibilityState={{ expanded: open }}
        onPress={() => keepTop(() => setOpen((value) => !value))}
        style={({ pressed }) => [styles.changedHeader, { opacity: pressed ? 0.6 : 1 }]}>
        <AppSymbol
          name={{ ios: 'plusminus', android: 'difference', web: 'difference' }}
          size={13}
          tintColor={theme.textSecondary}
        />
        <Text style={[styles.changedTitle, { color: theme.textSecondary }]}>
          {checkpoint.files.length} file{checkpoint.files.length === 1 ? '' : 's'} changed
        </Text>
        <Text style={styles.changedStats}>
          <Text style={{ color: theme.success }}>+{checkpoint.additions}</Text>
          <Text style={{ color: theme.textGhost }}> </Text>
          <Text style={{ color: theme.danger }}>−{checkpoint.deletions}</Text>
        </Text>
        <AppSymbol
          name={open
            ? { ios: 'chevron.up', android: 'keyboard_arrow_up', web: 'keyboard_arrow_up' }
            : { ios: 'chevron.down', android: 'keyboard_arrow_down', web: 'keyboard_arrow_down' }}
          size={11}
          tintColor={theme.textGhost}
        />
      </Pressable>
      {open && (
        <View style={[styles.changedFiles, { borderTopColor: theme.border }]}>
          {checkpoint.files.map((file) => (
            <View key={file.path} style={styles.changedFileRow}>
              <Text numberOfLines={1} style={[styles.changedFilePath, { color: theme.text }]}>
                {file.path}
              </Text>
              <Text style={styles.changedStats}>
                <Text style={{ color: theme.success }}>+{file.additions}</Text>
                <Text style={{ color: theme.textGhost }}> </Text>
                <Text style={{ color: theme.danger }}>−{file.deletions}</Text>
              </Text>
            </View>
          ))}
        </View>
      )}
    </View>
  );
}

/** Working-indicator flavour words, rotated every 7s, seeded per chat. */
const FLAVOUR_WORDS = [
  'Thinking', 'Pondering', 'Scheming', 'Brewing', 'Weaving', 'Tinkering',
  'Musing', 'Composing', 'Sifting', 'Untangling', 'Distilling', 'Sketching',
  'Plotting', 'Riffing', 'Combobulating', 'Percolating', 'Marinating',
  'Noodling', 'Puzzling', 'Conjuring',
];

function flavourSeed(chatId: string): number {
  let hash = 0x811c9dc5;
  for (let ix = 0; ix < chatId.length; ix += 1) {
    hash ^= chatId.charCodeAt(ix);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash;
}

function formatElapsed(seconds: number): string {
  const clamped = Math.max(0, seconds);
  if (clamped < 60) return `${clamped}s`;
  return `${Math.floor(clamped / 60)}m ${clamped % 60}s`;
}

/** The live footer under the streaming tail: spinner, flavour word, elapsed. */
export function WorkingStrip({ session }: { session: AgentSession }) {
  const theme = useTheme();
  const startedAt = session.turns.at(-1)?.started_at ?? null;
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1_000));
  useEffect(() => {
    const timer = setInterval(() => setNow(Math.floor(Date.now() / 1_000)), 1_000);
    return () => clearInterval(timer);
  }, []);
  const elapsed = startedAt ? Math.max(0, now - startedAt) : 0;
  const word = session.status === 'waiting'
    ? 'Waiting for you'
    : `${FLAVOUR_WORDS[(flavourSeed(session.id) + Math.floor(elapsed / 7)) % FLAVOUR_WORDS.length]}…`;
  return (
    <View accessibilityLiveRegion="polite" style={styles.workingStrip}>
      <ActivityIndicator color={theme.textTertiary} size="small" />
      <Text style={[styles.workingText, { color: theme.textTertiary }]}>{word}</Text>
      {startedAt != null && session.status !== 'waiting' && (
        <Text style={[styles.workingElapsed, { color: theme.textGhost }]}>
          {formatElapsed(elapsed)}
        </Text>
      )}
    </View>
  );
}

/** Sits above the oldest mounted row while more history exists; the window
 * extends ahead of the reader so this is rarely on screen for long. */
export function EarlierIndicator() {
  const theme = useTheme();
  return (
    <View accessibilityLabel="Loading earlier messages" style={styles.earlier}>
      <ActivityIndicator color={theme.textGhost} size="small" />
    </View>
  );
}

export function SessionEmpty({
  loading,
  error,
  missing,
}: {
  loading: boolean;
  error: unknown;
  missing: boolean;
}) {
  const theme = useTheme();
  if (loading) return <ActivityIndicator color={theme.textTertiary} />;
  return (
    <View style={styles.empty}>
      <Text style={[styles.emptyTitle, { color: theme.text }]}>
        {missing ? 'Task not found' : error ? 'Couldn’t load this task' : 'No messages yet'}
      </Text>
      {Boolean(error) && (
        <Text style={[styles.emptyBody, { color: theme.textSecondary }]}>
          {error instanceof Error ? error.message : String(error)}
        </Text>
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  earlier: { alignItems: 'center', paddingVertical: 10 },
  userFrame: { alignItems: 'flex-end', gap: 6 },
  userBubble: {
    borderRadius: Radius.large,
    borderBottomRightRadius: 6,
    maxWidth: '88%',
    paddingHorizontal: 16,
    paddingVertical: 10,
  },
  userText: { fontSize: 14, lineHeight: 21 },
  messageFooter: { fontSize: 10.5, marginTop: 10 },
  systemFrame: { alignItems: 'center' },
  systemMessage: {
    borderRadius: Radius.pill,
    fontSize: 11.5,
    lineHeight: 16,
    overflow: 'hidden',
    paddingHorizontal: 11,
    paddingVertical: 5,
    textAlign: 'center',
  },
  attachments: { flexDirection: 'row', flexWrap: 'wrap', justifyContent: 'flex-end', gap: 8, maxWidth: '88%' },
  foldRow: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 9,
    minHeight: 28,
  },
  foldLine: { flex: 1, height: StyleSheet.hairlineWidth },
  foldLabel: { flexShrink: 1, fontSize: 12, fontWeight: '500' },
  changedCard: {
    borderRadius: Radius.medium,
    borderWidth: StyleSheet.hairlineWidth,
    overflow: 'hidden',
  },
  changedHeader: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 8,
    minHeight: 40,
    paddingHorizontal: 11,
  },
  changedTitle: { flex: 1, fontSize: 12.5, fontWeight: '600' },
  changedStats: { fontSize: 11.5, fontVariant: ['tabular-nums'], fontWeight: '600' },
  changedFiles: {
    borderTopWidth: StyleSheet.hairlineWidth,
    gap: 6,
    paddingHorizontal: 11,
    paddingVertical: 9,
  },
  changedFileRow: { alignItems: 'center', flexDirection: 'row', gap: 8 },
  changedFilePath: { flex: 1, fontFamily: MonoFont, fontSize: 11 },
  workingStrip: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 8,
    paddingBottom: 8,
    paddingTop: 14,
  },
  workingText: { fontSize: 12.5, fontWeight: '500' },
  workingElapsed: { fontSize: 12, fontVariant: ['tabular-nums'] },
  empty: { alignItems: 'center', paddingHorizontal: 32 },
  emptyTitle: { fontSize: 17, fontWeight: '700', textAlign: 'center' },
  emptyBody: { fontSize: 13.5, lineHeight: 19, marginTop: 8, textAlign: 'center' },
});
