import {
  BottomSheetModal,
  BottomSheetView,
  type BottomSheetMethods,
} from '@expo/ui/community/bottom-sheet';
import type { ActivityFileChange, ActivityItem, AgentSession } from '@waku/client';
import { activitiesForBlock } from '@waku/client/event-reducer';
import {
  activityActionLabel,
  activityDisclosureSections,
  activityDisplayTitle,
  activityFileChangeStats,
  activityPreview,
  activityRowDetail,
  reasoningTitle,
} from '@waku/client/transcript-presentation';
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useReducer,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { Image, Platform, Pressable, ScrollView, StyleSheet, Text, View } from 'react-native';
import Animated, {
  Easing,
  useAnimatedStyle,
  useSharedValue,
  withTiming,
} from 'react-native-reanimated';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { ACTIVITY_ICONS } from './activity-icons';
import { AppSymbol } from './app-symbol';
import { DiffView } from './diff-view';
import { liquidGlass } from './glass-surface';
import { MonoFont, NativeTint, Radius, Spacing } from '@/constants/theme';
import { useReducedMotion } from '@/hooks/use-reduced-motion';
import { useTheme } from '@/hooks/use-theme';
import { findActivityBlock, type ActivityGroupTarget } from '@/lib/session-presentation';
import { applyAlpha } from '@/md/color';
import { RowVeil, splitRunAtSpans } from '@/md/veil';

type OpenActivityGroup = (target: ActivityGroupTarget) => void;

const ActivitySheetContext = createContext<OpenActivityGroup | null>(null);

function noop() {}

/** The opener for the activity sheet. Outside a host (tests) it is a no-op,
 * so transcript rows never need to know whether a sheet is mounted. */
export function useActivitySheet(): OpenActivityGroup {
  return useContext(ActivitySheetContext) ?? noop;
}

/** Whether the detail page would have anything to show for this activity —
 * the same test the page applies, so a tappable row never opens empty. */
function activityHasDetail(activity: ActivityItem): boolean {
  if (activity.reasoning) return activity.reasoning.content.trim().length > 0;
  return Boolean(
    activityDisclosureSections(activity).length
      || activity.file_changes?.length
      || activity.image_urls?.length,
  );
}

/** iOS gets the system medium/large detents. Android and web need explicit
 * snap points to offer a half-height state at all. */
const SNAP_POINTS = Platform.OS === 'ios' ? undefined : ['50%', '100%'];

/**
 * A tool group opens in a native bottom sheet instead of unfolding in the
 * transcript: the first page lists the group's activities, and choosing one
 * slides across to its detail (command, output, diffs, thinking) with a back
 * row — the same two-page pattern as the model picker. One sheet serves the
 * whole transcript. A summary row hands it a locator, and the body
 * re-resolves that against the freshest session on every commit (each
 * commit deep-clones the session), so a running group keeps growing and a
 * running command's output keeps streaming while the sheet is up.
 */
export function ActivitySheetHost({
  session,
  children,
}: {
  session: AgentSession;
  children: ReactNode;
}) {
  const theme = useTheme();
  const reducedMotion = useReducedMotion();
  const sheet = useRef<BottomSheetMethods>(null);
  const [target, setTarget] = useState<ActivityGroupTarget | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const open = useCallback<OpenActivityGroup>((next) => {
    setSelectedId(null);
    setTarget(next);
  }, []);

  const block = target ? findActivityBlock(session, target) : null;
  const activities = block ? activitiesForBlock(block) : [];
  const selected = selectedId
    ? activities.find((activity) => activity.id === selectedId) ?? null
    : null;

  // 0 = the group's list, 1 = one activity's detail.
  const progress = useSharedValue(0);
  const pageWidth = useSharedValue(0);
  const slideStyle = useAnimatedStyle(() => ({
    transform: [{ translateX: -pageWidth.value * progress.value }],
  }));
  function slideTo(page: 0 | 1) {
    progress.value = reducedMotion
      ? page
      : withTiming(page, { duration: 260, easing: Easing.bezier(0.32, 0.72, 0.25, 1) });
  }

  useEffect(() => {
    if (!target) return;
    progress.value = 0;
    sheet.current?.present();
  }, [progress, target]);

  // A rewind can drop the turn the open group belongs to; close rather than
  // leave an empty sheet up.
  useEffect(() => {
    if (target && !block) sheet.current?.dismiss();
  }, [block, target]);

  return (
    <ActivitySheetContext.Provider value={open}>
      {children}
      <BottomSheetModal
        ref={sheet}
        backgroundStyle={liquidGlass ? undefined : { backgroundColor: theme.surface }}
        enableDynamicSizing={false}
        enablePanDownToClose
        snapPoints={SNAP_POINTS}
        onDismiss={() => {
          setTarget(null);
          setSelectedId(null);
        }}>
        <BottomSheetView style={styles.fill}>
          {block ? (
            <View
              style={styles.pagerClip}
              onLayout={(event) => {
                pageWidth.value = event.nativeEvent.layout.width;
              }}>
              <Animated.View style={[styles.pagerTrack, slideStyle]}>
                <View style={styles.page}>
                  <GroupList
                    activities={activities}
                    onSelect={(id) => {
                      setSelectedId(id);
                      slideTo(1);
                    }}
                  />
                </View>
                <View style={styles.page}>
                  {selected ? (
                    <>
                      <Pressable
                        accessibilityHint="Shows the whole group"
                        accessibilityRole="button"
                        onPress={() => slideTo(0)}
                        style={({ pressed }) => [styles.backRow, { opacity: pressed ? 0.55 : 1 }]}>
                        <AppSymbol
                          name={{ ios: 'chevron.left', android: 'arrow_back', web: 'arrow_back' }}
                          size={14}
                          tintColor={NativeTint}
                        />
                        <Text style={[styles.backLabel, { color: NativeTint }]}>Back</Text>
                      </Pressable>
                      <ActivityDetail activity={selected} />
                    </>
                  ) : null}
                </View>
              </Animated.View>
            </View>
          ) : null}
        </BottomSheetView>
      </BottomSheetModal>
    </ActivitySheetContext.Provider>
  );
}

/** Page one: the group's activities as a picker-style list. The rows say
 * what happened; no headline restates it. */
function GroupList({
  activities,
  onSelect,
}: {
  activities: ActivityItem[];
  onSelect: (id: string) => void;
}) {
  const insets = useSafeAreaInsets();
  return (
    <ScrollView
      alwaysBounceVertical={false}
      contentContainerStyle={[styles.listContent, { paddingBottom: Math.max(insets.bottom, 16) + 8 }]}
      style={styles.fill}>
      {activities.map((activity) => (
        <GroupRow activity={activity} key={activity.id} onPress={() => onSelect(activity.id)} />
      ))}
    </ScrollView>
  );
}

/** One activity in the list: the desktop card's header — kind glyph, bold
 * action label, subject, edit totals, trailing state. */
function GroupRow({ activity, onPress }: { activity: ActivityItem; onPress: () => void }) {
  const theme = useTheme();
  const hasDetail = activityHasDetail(activity);
  const preview = activity.reasoning ? '' : activityPreview(activity);
  const detail = activityRowDetail(activity) || preview;
  const stats = activityFileChangeStats(activity);
  return (
    <Pressable
      accessibilityHint={hasDetail ? 'Opens the details' : undefined}
      accessibilityRole={hasDetail ? 'button' : 'text'}
      disabled={!hasDetail}
      onPress={onPress}
      style={({ pressed }) => [
        styles.row,
        { backgroundColor: pressed ? theme.overlayStrong : 'transparent' },
      ]}>
      <View style={[styles.rowIcon, { backgroundColor: theme.overlay }]}>
        <AppSymbol
          name={ACTIVITY_ICONS[activity.kind]}
          size={14}
          tintColor={theme.textSecondary}
        />
      </View>
      <Text numberOfLines={1} style={[styles.rowLabel, { color: theme.text }]}>
        <Text style={[styles.rowAction, { color: activity.failed ? theme.danger : theme.text }]}>
          {activityActionLabel(activity)}
        </Text>
        {detail ? <Text style={{ color: theme.textSecondary }}>{`  ${detail}`}</Text> : null}
      </Text>
      {stats && (
        <Text style={styles.rowStats}>
          <Text style={{ color: theme.success }}>+{stats.additions}</Text>
          <Text style={{ color: theme.textGhost }}> </Text>
          <Text style={{ color: theme.danger }}>−{stats.deletions}</Text>
        </Text>
      )}
      <RowState activity={activity} hasDetail={hasDetail} />
    </Pressable>
  );
}

/** Mirrors the desktop row's trailing state: a disclosure chevron when there
 * is detail to open, nothing for finished reasoning, an alert for failures,
 * a dot while live. */
function RowState({ activity, hasDetail }: { activity: ActivityItem; hasDetail: boolean }) {
  const theme = useTheme();
  if (hasDetail) {
    return (
      <AppSymbol
        name={{ ios: 'chevron.right', android: 'chevron_right', web: 'chevron_right' }}
        size={11}
        tintColor={theme.textGhost}
      />
    );
  }
  if (activity.reasoning) return null;
  if (activity.failed) {
    return (
      <AppSymbol
        name={{ ios: 'exclamationmark.triangle.fill', android: 'warning', web: 'warning' }}
        size={13}
        tintColor={theme.danger}
      />
    );
  }
  if (activity.complete) return null;
  return <View accessibilityLabel="Running" style={[styles.runningDot, { backgroundColor: NativeTint }]} />;
}

/** Page two: everything the desktop shows in an expanded card. */
function ActivityDetail({ activity }: { activity: ActivityItem }) {
  const theme = useTheme();
  const insets = useSafeAreaInsets();
  const reasoning = activity.reasoning ? activity.reasoning.content.trim() : '';
  const sections = activity.reasoning ? [] : activityDisclosureSections(activity);
  const changes = activity.file_changes ?? [];
  const images = activity.image_urls ?? [];
  const title = activity.reasoning ? reasoningTitle(activity) : activityDisplayTitle(activity);
  return (
    <ScrollView
      alwaysBounceVertical={false}
      contentContainerStyle={[styles.content, { paddingBottom: Math.max(insets.bottom, 16) + 8 }]}
      style={styles.fill}>
      <View style={styles.header}>
        <View style={[styles.iconWell, { backgroundColor: theme.overlayStrong }]}>
          <AppSymbol
            name={ACTIVITY_ICONS[activity.kind]}
            size={17}
            tintColor={theme.textSecondary}
          />
        </View>
        <View style={styles.headerCopy}>
          <Text
            accessibilityRole="header"
            numberOfLines={3}
            selectable
            style={[styles.title, { color: theme.text }]}>
            {title}
          </Text>
          <ActivityStatus activity={activity} />
        </View>
      </View>
      {activity.reasoning ? (
        <ReasoningText content={plainReasoning(reasoning)} live={!activity.complete} />
      ) : (
        <>
          {sections.map((section) => (
            <DetailSection
              content={section.content}
              key={section.kind}
              label={section.label}
              mono={section.kind !== 'detail'}
            />
          ))}
          {changes.map((change) => (
            <FileChange change={change} key={change.path} />
          ))}
          {images.map((url, index) => (
            <Image
              accessibilityIgnoresInvertColors
              key={index}
              resizeMode="contain"
              source={{ uri: url }}
              style={[styles.image, { backgroundColor: theme.inset }]}
            />
          ))}
        </>
      )}
    </ScrollView>
  );
}

/** Failure, progress, and edit totals under the title — each paired with a
 * word or glyph, never carried by color alone. */
function ActivityStatus({ activity }: { activity: ActivityItem }) {
  const theme = useTheme();
  if (activity.failed) {
    return (
      <View style={styles.status}>
        <AppSymbol
          name={{ ios: 'exclamationmark.triangle.fill', android: 'warning', web: 'warning' }}
          size={12}
          tintColor={theme.danger}
        />
        <Text style={[styles.statusText, { color: theme.danger }]}>Failed</Text>
      </View>
    );
  }
  // A live thought already says "Thinking" in its title.
  if (!activity.complete && !activity.reasoning) {
    return (
      <View accessibilityLiveRegion="polite" style={styles.status}>
        <View style={[styles.runningDot, { backgroundColor: NativeTint }]} />
        <Text style={[styles.statusText, { color: theme.textTertiary }]}>In progress</Text>
      </View>
    );
  }
  const stats = activityFileChangeStats(activity);
  if (stats) {
    return (
      <Text style={[styles.statusText, styles.stats]}>
        <Text style={{ color: theme.success }}>+{stats.additions}</Text>
        <Text style={{ color: theme.textGhost }}> </Text>
        <Text style={{ color: theme.danger }}>−{stats.deletions}</Text>
      </Text>
    );
  }
  return null;
}

function DetailSection({
  label,
  content,
  mono,
}: {
  label: string | null;
  content: string;
  mono: boolean;
}) {
  const theme = useTheme();
  return (
    <View style={styles.section}>
      {label ? (
        <Text style={[styles.sectionLabel, { color: theme.textTertiary }]}>{label}</Text>
      ) : null}
      {content ? (
        mono ? (
          <View style={[styles.well, { backgroundColor: theme.inset, borderColor: theme.border }]}>
            <Text selectable style={[styles.monoText, { color: theme.textSecondary }]}>
              {boundedText(content)}
            </Text>
          </View>
        ) : (
          <Text selectable style={[styles.bodyText, { color: theme.textSecondary }]}>
            {boundedText(content)}
          </Text>
        )
      ) : null}
    </View>
  );
}

function FileChange({ change }: { change: ActivityFileChange }) {
  const theme = useTheme();
  const status = change.status ?? 'modified';
  const statusColor = status === 'added'
    ? theme.success
    : status === 'deleted'
      ? theme.danger
      : theme.warning;
  const statusWord = status === 'added' ? 'Added' : status === 'deleted' ? 'Deleted' : 'Modified';
  const diff = change.diff?.trim() ? change.diff : null;
  return (
    <View style={styles.fileChange}>
      <View accessible accessibilityLabel={`${statusWord} ${change.path}`} style={styles.fileRow}>
        <View style={[styles.statusDot, { backgroundColor: statusColor }]} />
        <Text ellipsizeMode="head" numberOfLines={1} style={[styles.filePath, { color: theme.text }]}>
          {change.path}
        </Text>
        {(change.additions != null || change.deletions != null) && (
          <Text style={[styles.statusText, styles.stats]}>
            <Text style={{ color: theme.success }}>+{change.additions ?? 0}</Text>
            <Text style={{ color: theme.textGhost }}> </Text>
            <Text style={{ color: theme.danger }}>−{change.deletions ?? 0}</Text>
          </Text>
        )}
      </View>
      {diff ? <DiffView diff={diff} /> : null}
    </View>
  );
}

/** Streaming reasoning dissolves in like the desktop's strided reasoning
 * veil: appended text fades at half the message veil's tick rate, and text
 * present at mount is adopted at full opacity. */
function ReasoningText({ content, live }: { content: string; live: boolean }) {
  const theme = useTheme();
  const reducedMotion = useReducedMotion();
  const veil = useRef<RowVeil | null>(null);
  veil.current ??= new RowVeil(content.length > 0);
  const [, bump] = useReducer((count: number) => count + 1, 0);
  const spans = live && !reducedMotion ? veil.current.advance(0, content, Date.now()) : [];
  useEffect(() => {
    if (!spans.length) return;
    const timer = setTimeout(bump, 66);
    return () => clearTimeout(timer);
  });
  return (
    <Text selectable style={[styles.bodyText, { color: theme.textSecondary }]}>
      {splitRunAtSpans(0, content.length, spans).map(([start, end, opacity]) =>
        opacity >= 1 ? (
          content.slice(start, end)
        ) : (
          <Text key={start} style={{ color: applyAlpha(theme.textSecondary, opacity) }}>
            {content.slice(start, end)}
          </Text>
        ),
      )}
    </Text>
  );
}

function boundedText(value: string): string {
  const limit = 12_000;
  return value.length <= limit ? value : `${value.slice(0, limit)}\n\n… Output truncated`;
}

/** Reasoning is throwaway thinking: render it as quiet plain text, never
 * heavier than the answer. Strips the markdown emphasis and heading markers
 * providers put on their summary headlines. */
function plainReasoning(value: string): string {
  return boundedText(value)
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/__([^_]+)__/g, '$1')
    .replace(/^#{1,6}\s+/gm, '');
}

const styles = StyleSheet.create({
  fill: { flex: 1 },
  pagerClip: { flex: 1, overflow: 'hidden' },
  pagerTrack: { flex: 1, flexDirection: 'row', width: '200%' },
  page: { width: '50%' },
  // ── Page one: the list ──────────────────────────────────────────────────
  listContent: { paddingHorizontal: Spacing.two, paddingTop: 6 },
  row: {
    alignItems: 'center',
    borderRadius: Radius.medium,
    flexDirection: 'row',
    gap: 11,
    minHeight: 46,
    paddingHorizontal: 12,
    paddingVertical: 8,
  },
  rowIcon: {
    alignItems: 'center',
    borderRadius: Radius.tiny,
    height: 26,
    justifyContent: 'center',
    width: 26,
  },
  rowLabel: { flex: 1, fontSize: 15, minWidth: 0 },
  rowAction: { fontWeight: '600' },
  rowStats: { fontSize: 12, fontVariant: ['tabular-nums'], fontWeight: '600' },
  runningDot: { borderRadius: 3, height: 6, width: 6 },
  // ── Page two: the detail ────────────────────────────────────────────────
  backRow: {
    alignItems: 'center',
    flexDirection: 'row',
    gap: 5,
    marginHorizontal: Spacing.two,
    minHeight: 40,
    paddingHorizontal: 10,
  },
  backLabel: { fontSize: 15, fontWeight: '600' },
  content: { gap: 14, paddingHorizontal: Spacing.three, paddingTop: 6 },
  header: { alignItems: 'flex-start', flexDirection: 'row', gap: 12 },
  iconWell: {
    alignItems: 'center',
    borderRadius: Radius.small,
    height: 34,
    justifyContent: 'center',
    width: 34,
  },
  headerCopy: { flex: 1, gap: 3, minWidth: 0 },
  title: { fontSize: 17, fontWeight: '600', lineHeight: 22 },
  status: { alignItems: 'center', flexDirection: 'row', gap: 6 },
  statusText: { fontSize: 13 },
  stats: { fontVariant: ['tabular-nums'], fontWeight: '600' },
  section: { gap: 6 },
  sectionLabel: {
    fontSize: 11.5,
    fontWeight: '600',
    letterSpacing: 0.4,
    textTransform: 'uppercase',
  },
  well: {
    borderRadius: Radius.small,
    borderWidth: StyleSheet.hairlineWidth,
    paddingHorizontal: 10,
    paddingVertical: 8,
  },
  monoText: { fontFamily: MonoFont, fontSize: 12, lineHeight: 17.5 },
  bodyText: { fontSize: 14.5, lineHeight: 21 },
  fileChange: { gap: 8 },
  fileRow: { alignItems: 'center', flexDirection: 'row', gap: 8, minHeight: 24 },
  statusDot: { borderRadius: 3, height: 6, width: 6 },
  filePath: { flex: 1, fontFamily: MonoFont, fontSize: 12 },
  image: { borderRadius: Radius.small, height: 240, width: '100%' },
});
