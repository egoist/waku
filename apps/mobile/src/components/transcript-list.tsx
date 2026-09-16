import type { AgentSession } from '@waku/client';
import {
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type Ref,
} from 'react';
import {
  Animated,
  Keyboard,
  Pressable,
  ScrollView,
  StyleSheet,
  View,
  type LayoutChangeEvent,
  type NativeScrollEvent,
  type NativeSyntheticEvent,
} from 'react-native';
import { ScrollViewMarker } from 'react-native-screens/experimental';

import { AppSymbol } from '@/components/app-symbol';
import { GlassSurface } from '@/components/glass-surface';
import { useMarkdownStyles, VeilRegistry } from '@/components/md-block-row';
import { RowAnchorProvider } from '@/components/transcript-anchor';
import {
  EarlierIndicator,
  SessionEmpty,
  TranscriptRowView,
  WorkingStrip,
} from '@/components/transcript-rows';
import { Radius, Spacing } from '@/constants/theme';
import { useReducedMotion } from '@/hooks/use-reduced-motion';
import { useTheme } from '@/hooks/use-theme';
import {
  buildTranscriptPipeline,
  expandTranscriptRows,
  stabilizeTranscriptRows,
  type TranscriptRow,
} from '@/lib/session-presentation';
import {
  extendedWindowStart,
  initialWindowStart,
  jumpVisible,
  JUMP_HIDE_DISTANCE,
  JUMP_SHOW_DISTANCE,
  liveSkipIndex,
  nativeChildIndex,
  offTail,
  shouldExtendTail,
  underHeader,
  peekedWithinBand,
  type ScrollMetrics,
} from '@/lib/transcript-scroll';
import type { MarkdownStyles } from '@/md/render';
import { TranscriptMarkdownCache } from '@/md/transcript-cache';

/** Content column cap (iPad). */
const MAX_CONTENT_WIDTH = 736;

/** Longer than UIScrollView's animated scroll (~0.3 s) plus a frame. */
const SEAT_GLIDE_MS = 700;

export interface TranscriptListHandle {
  scrollToLatest: (animated?: boolean) => void;
  /** A message the reader just sent is about to land: follow it to the
   * bottom even if they had scrolled up into history. */
  followNextGrowth: () => void;
}

export interface TranscriptDevSample {
  at: number;
  offset: number;
  contentHeight: number;
  touching: boolean;
  source: 'scroll' | 'size';
}

type KeepRowTop = (rowKey: string, apply: () => void) => void;

/**
 * The transcript.
 *
 * An inverted native ScrollView: rows mount newest-first inside a scaleY(-1)
 * container, so native offset 0 is the visual bottom and the streaming tail
 * grows at native y = 0. Following a stream is therefore a property of the
 * layout — no scroll command, no measurement round-trip, no correction
 * frame. `maintainVisibleContentPosition` keeps a reader who left the tail
 * on their line: the anchor is pointed past every row of the running turn,
 * so growth is compensated atomically even when it happens inside one tall
 * row, which a child-based anchor cannot otherwise see into.
 *
 * JS owns only what the native layer cannot know: which of the two anchors
 * applies, the re-seat for a reader resting just off the bottom, the
 * jump-to-latest button, the header backdrop, mounting more history, and
 * the anchor handshake that makes tapped disclosures grow downward.
 */
export function TranscriptList({
  ref,
  session,
  hydrated,
  running,
  headerInset,
  onUnderHeaderChange,
  onDevSample,
}: {
  ref?: Ref<TranscriptListHandle>;
  session: AgentSession;
  /** False while the session is a task-list skeleton awaiting hydration. */
  hydrated: boolean;
  running: boolean;
  headerInset: number;
  onUnderHeaderChange: (under: boolean) => void;
  onDevSample?: (sample: TranscriptDevSample) => void;
}) {
  const theme = useTheme();
  const reducedMotion = useReducedMotion();
  const markdownStyles = useMarkdownStyles();
  const scrollRef = useRef<ScrollView>(null);

  // Parse + veil state live as long as this list does (keyed per session).
  const [md] = useState(() => new TranscriptMarkdownCache());
  const [veils] = useState(() => new VeilRegistry());

  // ── Rows ────────────────────────────────────────────────────────────────
  const [expandedFolds, setExpandedFolds] = useState<ReadonlySet<string>>(() => new Set());

  const runningTurnId = session.turns.find((turn) => turn.status === 'running')?.id ?? null;
  const pipeline = useMemo(
    () => buildTranscriptPipeline(session, expandedFolds),
    [expandedFolds, session],
  );

  // The mounted window: pipeline rows from `windowStart` to the end. Fixed
  // once the real transcript is known so streaming never shifts it, and only
  // ever grows toward older history — new rows mount at the native end where
  // they cannot move the offset.
  const [windowStart, setWindowStart] = useState<number | null>(
    () => hydrated ? initialWindowStart(pipeline.length) : null,
  );
  if (hydrated && windowStart === null) setWindowStart(initialWindowStart(pipeline.length));
  const start = Math.min(windowStart ?? initialWindowStart(pipeline.length), pipeline.length);
  const hasEarlier = start > 0;

  const freshRows = useMemo(() => expandTranscriptRows(pipeline, md, start), [md, pipeline, start]);
  const rows = useStableRows(freshRows);

  // Messages already present when this list first rendered (or hydrated)
  // show their markdown at full opacity — only content streamed while
  // watching dissolves in.
  const [seeded, setSeeded] = useState<{ phase: boolean; ids: ReadonlySet<string> }>(() => ({
    phase: hydrated,
    ids: new Set(session.messages.map((message) => message.id)),
  }));
  if (seeded.phase !== hydrated) {
    const ids = new Set(seeded.ids);
    for (const message of session.messages) ids.add(message.id);
    setSeeded({ phase: hydrated, ids });
  }

  const toggleFold = useCallback((turnId: string) => {
    setExpandedFolds((current) => {
      const next = new Set(current);
      if (next.has(turnId)) next.delete(turnId);
      else next.add(turnId);
      return next;
    });
  }, []);

  // ── Anchor handshake ────────────────────────────────────────────────────
  // Native children: [strip, ...rows newest-first, top sentinel]. A row's
  // older neighbour (index + 1) starts where the row's visual top is, so
  // anchoring the neighbour for one commit pins the tapped header in place
  // while the row grows downward. Three commits: arm, apply, disarm — the
  // native prepare step reads the props of the previous mount, so the arm
  // must land a frame before the change, and the disarm a frame after.
  const [anchorIndex, setAnchorIndex] = useState<number | null>(null);
  const pendingApply = useRef<(() => void) | null>(null);
  const nativeIndexByKey = useMemo(() => {
    const map = new Map<string, number>();
    rows.forEach((row, index) => map.set(row.key, nativeChildIndex(index, rows.length)));
    return map;
  }, [rows]);
  const nativeIndexRef = useRef(nativeIndexByKey);
  useEffect(() => {
    nativeIndexRef.current = nativeIndexByKey;
  }, [nativeIndexByKey]);

  const keepRowTop = useCallback<KeepRowTop>((rowKey, apply) => {
    const index = nativeIndexRef.current.get(rowKey);
    if (index == null || pendingApply.current) {
      apply();
      return;
    }
    pendingApply.current = apply;
    setAnchorIndex(index + 1);
  }, []);

  useEffect(() => {
    if (anchorIndex == null) return;
    let frame = requestAnimationFrame(() => {
      const apply = pendingApply.current;
      pendingApply.current = null;
      apply?.();
      frame = requestAnimationFrame(() => setAnchorIndex(null));
    });
    return () => cancelAnimationFrame(frame);
  }, [anchorIndex]);

  // ── Scroll bookkeeping ──────────────────────────────────────────────────
  const metrics = useRef<ScrollMetrics>({ offset: 0, contentHeight: 0, viewportHeight: 0 });
  const underRef = useRef(false);
  const touchingRef = useRef(false);
  const touchMoved = useRef(false);
  const extending = useRef(false);
  const decelerating = useRef(false);
  /** Where the current gesture began: its offset and the content height,
   * so growth compensated under the finger can be told from the reader's
   * own movement when the gesture settles. */
  const touchOrigin = useRef<{ offset: number; contentHeight: number } | null>(null);
  const followRequest = useRef<ReturnType<typeof setTimeout> | null>(null);
  const jumpRef = useRef(false);
  const unseenRef = useRef(false);
  const offTailRef = useRef(false);
  const [touching, setTouching] = useState(false);
  const [readerOffTail, setReaderOffTail] = useState(false);
  const [jump, setJump] = useState(false);
  const [unseen, setUnseen] = useState(false);

  /** While a glide to the tail is in flight the reader is treated as seated:
   * the anchor moves to the strip before the animation starts, so growth
   * during the glide lands at the target instead of compensating against
   * it (a compensation fight showed up as ~1700 scroll events/s). */
  const seatingUntil = useRef(0);

  const scrollToLatest = useCallback((animated = true) => {
    seatingUntil.current = Date.now() + SEAT_GLIDE_MS;
    if (offTailRef.current) {
      offTailRef.current = false;
      setReaderOffTail(false);
    }
    if (jumpRef.current) {
      jumpRef.current = false;
      setJump(false);
    }
    scrollRef.current?.scrollTo({ y: 0, animated: animated && !reducedMotion });
  }, [reducedMotion]);

  const evaluate = useCallback(() => {
    const current = metrics.current;
    const under = underHeader(current);
    if (under !== underRef.current) {
      underRef.current = under;
      onUnderHeaderChange(under);
    }
    // Scroll events arrive at display rate; touch React state on transitions
    // only, and not at all mid-glide — the offset is on its way to 0.
    const gliding = Date.now() < seatingUntil.current && !touchingRef.current;
    const nextOffTail = !gliding && offTail(current.offset);
    if (nextOffTail !== offTailRef.current) {
      offTailRef.current = nextOffTail;
      setReaderOffTail(nextOffTail);
    }
    const nextJump = !gliding && jumpVisible(current.offset, jumpRef.current);
    if (nextJump !== jumpRef.current) {
      jumpRef.current = nextJump;
      setJump(nextJump);
    }
    if (current.offset <= JUMP_HIDE_DISTANCE && unseenRef.current) {
      unseenRef.current = false;
      setUnseen(false);
    }
    if (!extending.current && shouldExtendTail(current, hasEarlier)) {
      extending.current = true;
      setWindowStart((value) => extendedWindowStart(value ?? start));
    }
  }, [hasEarlier, onUnderHeaderChange, start]);

  // A window extension that changes nothing on screen (all-hidden rows) must
  // not wedge the extender.
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      extending.current = false;
      evaluate();
    });
    return () => cancelAnimationFrame(frame);
  }, [evaluate, start]);

  function onScroll(event: NativeSyntheticEvent<NativeScrollEvent>) {
    const { contentOffset, contentSize, layoutMeasurement } = event.nativeEvent;
    metrics.current = {
      offset: contentOffset.y,
      contentHeight: contentSize.height,
      viewportHeight: layoutMeasurement.height,
    };
    onDevSample?.({
      at: Date.now(),
      offset: contentOffset.y,
      contentHeight: contentSize.height,
      touching: touchingRef.current,
      source: 'scroll',
    });
    evaluate();
  }

  function onLayout(event: LayoutChangeEvent) {
    metrics.current = { ...metrics.current, viewportHeight: event.nativeEvent.layout.height };
    evaluate();
  }

  /** A reader resting just off the bottom is brought back to it — never
   * under a finger, never through a fling. The band is judged by what the
   * reader did, not by what the stream did underneath them. The target is
   * always 0, so a re-seat can neither chase nor fight the stream. */
  const reseatIfInBand = useCallback(() => {
    if (touchingRef.current || decelerating.current) return;
    const { offset, contentHeight } = metrics.current;
    const origin = touchOrigin.current;
    const grown = origin ? Math.max(0, contentHeight - origin.contentHeight) : 0;
    if (peekedWithinBand(offset, offset - grown)) scrollToLatest(true);
  }, [scrollToLatest]);

  /** The gesture is over (tap, drag without fling, or momentum end): decide
   * once, then forget its origin so later growth is judged on its own. */
  const settleGesture = useCallback(() => {
    reseatIfInBand();
    touchOrigin.current = null;
  }, [reseatIfInBand]);

  function onContentSizeChange(_width: number, height: number) {
    const grew = height > metrics.current.contentHeight + 0.5;
    metrics.current = { ...metrics.current, contentHeight: height };
    extending.current = false;
    onDevSample?.({
      at: Date.now(),
      offset: metrics.current.offset,
      contentHeight: height,
      touching: touchingRef.current,
      source: 'size',
    });
    if (followRequest.current) {
      clearTimeout(followRequest.current);
      followRequest.current = null;
      scrollToLatest(true);
    } else if (grew && metrics.current.offset > JUMP_SHOW_DISTANCE && !unseenRef.current) {
      unseenRef.current = true;
      setUnseen(true);
    } else if (grew) {
      reseatIfInBand();
    }
    evaluate();
  }

  function onMomentumScrollBegin() {
    decelerating.current = true;
  }

  function onMomentumScrollEnd() {
    decelerating.current = false;
    releaseTouch();
    settleGesture();
  }

  function onScrollEndDrag(event: NativeSyntheticEvent<NativeScrollEvent>) {
    // The native pan owns the finger once it starts; its end is the
    // authoritative "finger up" even if the JS touch end never arrives.
    releaseTouch();
    // With no fling the momentum callbacks never fire; settle here.
    const velocity = event.nativeEvent.velocity?.y ?? 0;
    if (Math.abs(velocity) < 0.1) settleGesture();
  }

  const followNextGrowth = useCallback(() => {
    if (followRequest.current) clearTimeout(followRequest.current);
    followRequest.current = setTimeout(() => {
      followRequest.current = null;
    }, 1_500);
    scrollToLatest(true);
  }, [scrollToLatest]);

  useImperativeHandle(ref, () => ({ scrollToLatest, followNextGrowth }), [followNextGrowth, scrollToLatest]);

  useEffect(() => () => {
    if (followRequest.current) clearTimeout(followRequest.current);
  }, []);

  // Any touch owns the viewport: content must not move under a finger, so
  // the history anchor applies for the whole gesture. A plain tap also puts
  // the keyboard away, the way every iOS chat does.
  function onTouchStart() {
    touchingRef.current = true;
    touchMoved.current = false;
    seatingUntil.current = 0;
    touchOrigin.current = { offset: metrics.current.offset, contentHeight: metrics.current.contentHeight };
    setTouching(true);
  }

  function releaseTouch() {
    if (!touchingRef.current) return;
    touchingRef.current = false;
    setTouching(false);
  }

  function onTouchEnd() {
    releaseTouch();
    if (!touchMoved.current) {
      Keyboard.dismiss();
      settleGesture();
    }
  }

  function onScrollBeginDrag() {
    touchMoved.current = true;
    if (!touchOrigin.current) {
      touchOrigin.current = { offset: metrics.current.offset, contentHeight: metrics.current.contentHeight };
    }
    if (!touchingRef.current) {
      touchingRef.current = true;
      setTouching(true);
    }
  }

  // Two anchors. Seated on the tail: the strip at native index 0, whose
  // origin never moves, so the layout itself keeps the bottom in view.
  // Reading (off the tail or touching): the first row of settled history, so
  // every growth in the running turn — including inside a single tall row —
  // is compensated in the same native layout pass.
  const liveSkip = useMemo(() => liveSkipIndex(rows, runningTurnId), [rows, runningTurnId]);
  const maintainPosition = useMemo(() => ({
    minIndexForVisible: anchorIndex ?? ((touching || readerOffTail) ? liveSkip : 0),
  }), [anchorIndex, liveSkip, readerOffTail, touching]);

  // ── Jump button ─────────────────────────────────────────────────────────
  const [jumpOpacity] = useState(() => new Animated.Value(0));
  useEffect(() => {
    Animated.timing(jumpOpacity, {
      duration: reducedMotion ? 0 : 160,
      toValue: jump ? 1 : 0,
      useNativeDriver: true,
    }).start();
  }, [jump, jumpOpacity, reducedMotion]);

  const showEmpty = hydrated && !running && rows.length === 0;

  return (
    <View style={styles.frame}>
      <ScrollViewMarker
        scrollEdgeEffects={{ bottom: 'hidden', left: 'hidden', right: 'hidden', top: 'hidden' }}
        style={styles.list}>
        <ScrollView
          ref={scrollRef}
          contentContainerStyle={styles.content}
          contentInsetAdjustmentBehavior="never"
          keyboardDismissMode="interactive"
          keyboardShouldPersistTaps="handled"
          maintainVisibleContentPosition={maintainPosition}
          scrollEventThrottle={16}
          showsVerticalScrollIndicator={false}
          style={[styles.list, styles.inverted]}
          onContentSizeChange={onContentSizeChange}
          onLayout={onLayout}
          onMomentumScrollBegin={onMomentumScrollBegin}
          onMomentumScrollEnd={onMomentumScrollEnd}
          onScroll={onScroll}
          onScrollBeginDrag={onScrollBeginDrag}
          onScrollEndDrag={onScrollEndDrag}
          onTouchCancel={onTouchEnd}
          onTouchEnd={onTouchEnd}
          onTouchStart={onTouchStart}>
          {/* Child 0, always mounted: the native anchor while pinned. */}
          <View style={[styles.inverted, styles.column]}>
            {running ? <WorkingStrip session={session} /> : null}
          </View>
          {rows.map((_, reversed) => {
            const row = rows[rows.length - 1 - reversed]!;
            return (
              <TranscriptRowFrame
                key={row.key}
                keepRowTop={keepRowTop}
                markdownStyles={markdownStyles}
                md={md}
                row={row}
                seeded={row.kind === 'md' && seeded.ids.has(row.messageId)}
                veils={veils}
                onToggleFold={toggleFold}
              />
            );
          })}
          {/* Last child: the visual top, under the floating header. */}
          <View style={[styles.inverted, styles.column, { paddingTop: headerInset + 2 }]}>
            {hasEarlier && <EarlierIndicator />}
          </View>
        </ScrollView>
      </ScrollViewMarker>
      {showEmpty && (
        <View pointerEvents="none" style={styles.emptyOverlay}>
          <SessionEmpty error={null} loading={false} missing={false} />
        </View>
      )}
      <Animated.View
        pointerEvents={jump ? 'auto' : 'none'}
        style={[styles.jumpFrame, { opacity: jumpOpacity }]}>
        <GlassSurface fallbackColor={theme.surface} interactive style={styles.jumpButton}>
          <Pressable
            accessibilityHint={unseen ? 'New content arrived below' : undefined}
            accessibilityLabel="Scroll to latest"
            accessibilityRole="button"
            onPress={() => scrollToLatest(true)}
            style={({ pressed }) => [styles.jumpButtonInner, { opacity: pressed ? 0.6 : 1 }]}>
            <AppSymbol
              name={{ ios: 'arrow.down', android: 'arrow_downward', web: 'arrow_downward' }}
              size={15}
              tintColor={theme.textSecondary}
            />
            {unseen && <View style={[styles.unseenDot, { backgroundColor: theme.accent, borderColor: theme.surface }]} />}
          </Pressable>
        </GlassSurface>
      </Animated.View>
    </View>
  );
}

/** Reuse the previous render's row objects wherever nothing changed so the
 * memoized row views bail out. */
function useStableRows(fresh: TranscriptRow[]): TranscriptRow[] {
  const previous = useRef<TranscriptRow[]>([]);
  const rows = useMemo(() => stabilizeTranscriptRows(previous.current, fresh), [fresh]);
  useEffect(() => {
    previous.current = rows;
  }, [rows]);
  return rows;
}

/** The direct ScrollView child for one row: the unit the native anchor
 * tracks, flipped back upright inside the inverted list. */
const TranscriptRowFrame = memo(function TranscriptRowFrame({
  row,
  keepRowTop,
  md,
  veils,
  seeded,
  markdownStyles,
  onToggleFold,
}: {
  row: TranscriptRow;
  keepRowTop: KeepRowTop;
  md: TranscriptMarkdownCache;
  veils: VeilRegistry;
  seeded: boolean;
  markdownStyles: MarkdownStyles;
  onToggleFold: (turnId: string) => void;
}) {
  const keepTop = useCallback(
    (apply: () => void) => keepRowTop(row.key, apply),
    [keepRowTop, row.key],
  );
  return (
    <View style={[styles.inverted, styles.column, { paddingTop: row.topGap }]}>
      <RowAnchorProvider value={keepTop}>
        <TranscriptRowView
          markdownStyles={markdownStyles}
          md={md}
          row={row}
          seeded={seeded}
          veils={veils}
          onToggleFold={onToggleFold}
        />
      </RowAnchorProvider>
    </View>
  );
});

const styles = StyleSheet.create({
  frame: { flex: 1, overflow: 'hidden' },
  list: { flex: 1 },
  inverted: { transform: [{ scaleY: -1 }] },
  // Native padding-top is the visual bottom: breathing room above the composer.
  content: { paddingHorizontal: Spacing.three, paddingTop: 12 },
  column: { alignSelf: 'center', maxWidth: MAX_CONTENT_WIDTH, width: '100%' },
  emptyOverlay: {
    alignItems: 'center',
    bottom: 0,
    justifyContent: 'center',
    left: 0,
    position: 'absolute',
    right: 0,
    top: 0,
  },
  jumpFrame: {
    bottom: 12,
    position: 'absolute',
    right: 14,
  },
  jumpButton: {
    borderRadius: Radius.pill,
    height: 40,
    width: 40,
  },
  jumpButtonInner: { alignItems: 'center', flex: 1, justifyContent: 'center' },
  unseenDot: {
    borderRadius: 5,
    borderWidth: 1.5,
    height: 10,
    position: 'absolute',
    right: 7,
    top: 7,
    width: 10,
  },
});
