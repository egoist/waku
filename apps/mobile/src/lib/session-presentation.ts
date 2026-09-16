import type {
  AgentSession,
  AgentTurn,
  Checkpoint,
  Message,
  Project,
  ProviderKind,
  RuntimeMode,
  TranscriptBlock,
} from '@waku/client';
import { turnAnswerStart, turnFoldLabel } from '@waku/client/transcript-presentation';

import type { MarkdownBlock } from '../md/parse';
import { TranscriptMarkdownCache } from '../md/transcript-cache';

export type SessionGroupId = 'today' | 'yesterday' | 'week' | 'month' | 'year' | 'more';

export interface SessionListItem {
  session: AgentSession;
  projectName: string;
  timestamp: number;
}

export interface SessionGroup {
  id: SessionGroupId;
  title: string;
  data: SessionListItem[];
}

/**
 * One row = one markdown top-level block / user bubble / tool group / chip,
 * never one assistant message. A streamed
 * token re-renders exactly one row (the live tail block), and the list only
 * re-lays-out what changed. `topGap` is resolved at build time because it
 * depends on the previous row. `turnId` lets the list tell the running
 * turn's rows — the ones that grow in place while streaming — from settled
 * history.
 */
export type TranscriptRow =
  | { kind: 'user'; key: string; turnId: string | null; message: Message; topGap: number }
  | { kind: 'system'; key: string; turnId: string | null; message: Message; topGap: number }
  | {
      kind: 'md';
      key: string;
      turnId: string | null;
      messageId: string;
      blockIx: number;
      source: string;
      node: MarkdownBlock['node'];
      /** True only for the last block of the streaming message: it renders
       * the mended display tail with the veil, at display cadence. */
      live: boolean;
      /** True for every block of a message that is still streaming — freshly
       * minted settled rows fade in on mount while this holds. */
      streaming: boolean;
      footerTimestamp: number | null;
      topGap: number;
    }
  | {
      kind: 'activities';
      key: string;
      turnId: string | null;
      block: TranscriptBlock;
      /** Position in `session.transcript_blocks`: the sheet's locator hint. */
      blockIndex: number;
      live: boolean;
      topGap: number;
    }
  | {
      kind: 'fold';
      key: string;
      turnId: string | null;
      turn: AgentTurn;
      label: string;
      expanded: boolean;
      topGap: number;
    }
  | { kind: 'changed'; key: string; turnId: string | null; checkpoint: Checkpoint; topGap: number };

/** Spacing tokens: first row, message boundaries, tool boundaries,
 * sibling markdown blocks of one message. */
const GAP_FIRST = 26;
const GAP_TURN = 16;
const GAP_GROUP = 12;
const GAP_BLOCK = 12;

const GROUPS: Array<{ id: SessionGroupId; title: string }> = [
  { id: 'today', title: 'Today' },
  { id: 'yesterday', title: 'Yesterday' },
  { id: 'week', title: 'This Week' },
  { id: 'month', title: 'This Month' },
  { id: 'year', title: 'This Year' },
  { id: 'more', title: 'More' },
];

export function displaySessionTitle(session: AgentSession): string {
  if (session.title !== 'New task' && session.title.trim()) return session.title.trim();
  return session.auto_title?.trim() || 'New Task';
}

export function sessionHasStarted(session: AgentSession): boolean {
  return Boolean(
    session.turns.length || session.messages.length || session.provider_cursor || session.last_reply_at,
  );
}

export function sessionTimestamp(session: AgentSession): number {
  // Match desktop: a submitted/replied turn promotes the task, while metadata
  // edits such as rename leave it in its existing date group.
  return session.last_reply_at ?? session.created_at;
}

export function groupSessions(
  projects: Project[],
  sessions: AgentSession[],
  now = new Date(),
): SessionGroup[] {
  const projectNames = new Map(projects.map((project) => [project.id, project.name]));
  const grouped = new Map<SessionGroupId, SessionListItem[]>();
  for (const session of sessions.filter(sessionHasStarted).sort((a, b) => (
    sessionTimestamp(b) - sessionTimestamp(a)
  ))) {
    const id = sessionDateGroup(sessionTimestamp(session), now);
    const items = grouped.get(id) ?? [];
    items.push({
      session,
      projectName: projectNames.get(session.project_id) || 'Unknown project',
      timestamp: sessionTimestamp(session),
    });
    grouped.set(id, items);
  }
  return GROUPS.flatMap((group) => {
    const data = grouped.get(group.id);
    return data?.length ? [{ ...group, data }] : [];
  });
}

export function sessionDateGroup(timestamp: number, now = new Date()): SessionGroupId {
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const day = new Date(timestamp * 1_000);
  const dayStart = new Date(day.getFullYear(), day.getMonth(), day.getDate()).getTime();
  if (dayStart >= start) return 'today';

  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1).getTime();
  if (dayStart === yesterday) return 'yesterday';

  const weekStart = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  weekStart.setDate(weekStart.getDate() - ((weekStart.getDay() + 6) % 7));
  if (dayStart >= weekStart.getTime()) return 'week';
  if (day.getFullYear() === now.getFullYear() && day.getMonth() === now.getMonth()) {
    return 'month';
  }
  if (day.getFullYear() === now.getFullYear()) return 'year';
  return 'more';
}

export function relativeSessionTime(timestamp: number, now = Date.now()): string {
  const elapsed = Math.max(0, Math.floor(now / 1_000) - timestamp);
  if (elapsed < 60) return 'just now';
  if (elapsed < 3_600) return `${Math.floor(elapsed / 60)}m`;
  if (elapsed < 86_400) return `${Math.floor(elapsed / 3_600)}h`;
  return `${Math.floor(elapsed / 86_400)}d`;
}

export function providerLabel(provider: ProviderKind): string {
  const labels: Record<ProviderKind, string> = {
    amp: 'Amp',
    claude: 'Claude',
    codex: 'Codex',
    cursor: 'Cursor',
    deepSeek: 'DeepSeek',
    fx: 'Fx',
    openCode: 'OpenCode',
    openCode2: 'OpenCode 2',
    grok: 'Grok',
    kimi: 'Kimi',
    ohMyPi: 'Oh My Pi',
    pi: 'Pi',
  };
  return labels[provider];
}

export function runtimeModeLabel(mode: RuntimeMode): string {
  const labels: Record<RuntimeMode, string> = {
    ask: 'Ask first',
    autoAcceptEdits: 'Accept edits',
    auto: 'Auto',
    fullAccess: 'Full access',
  };
  return labels[mode];
}

export function contextPercent(session: AgentSession): number | null {
  const usage = session.context_usage;
  if (!usage || !usage.window) return null;
  return Math.max(0, Math.min(100, Math.round((usage.tokens / usage.window) * 100)));
}

/**
 * Locator for one tool group, as handed to the activity sheet. Every stream
 * commit deep-clones the session, so the sheet keeps this rather than the
 * block and re-resolves it against the freshest session on each render. The
 * index is a hint checked against the anchor; a block that moved (a rewind
 * dropped an earlier turn) is found again by its anchor.
 */
export interface ActivityGroupTarget {
  blockIndex: number;
  turnId: string | null;
  afterMessage: number;
}

export function findActivityBlock(
  session: AgentSession,
  target: ActivityGroupTarget,
): TranscriptBlock | null {
  const matches = (block: TranscriptBlock) =>
    block.turn_id === target.turnId && block.after_message === target.afterMessage;
  const hinted = session.transcript_blocks[target.blockIndex];
  if (hinted && matches(hinted)) return hinted;
  return session.transcript_blocks.find(matches) ?? null;
}

/**
 * Message-granular pipeline rows — the cheap, parse-free skeleton of the
 * transcript. Assistant messages expand into `md` block rows only for the
 * slice that is actually rendered (see `expandTranscriptRows`), so opening a
 * five-thousand-message session costs one linear pass, not a markdown parse
 * of every answer.
 */
export type TranscriptPipelineRow =
  | { kind: 'message'; key: string; turnId: string | null; message: Message; footerTimestamp: number | null }
  | { kind: 'activities'; key: string; turnId: string | null; block: TranscriptBlock; blockIndex: number; live: boolean }
  | { kind: 'fold'; key: string; turnId: string | null; turn: AgentTurn; label: string; expanded: boolean }
  | { kind: 'changed'; key: string; turnId: string | null; checkpoint: Checkpoint };

interface TaggedRow {
  row: TranscriptPipelineRow;
  turnId: string | null;
  foldable: boolean;
  answerText: boolean;
}

/**
 * Interleaves messages with activity blocks and, for every settled turn,
 * hides everything before the terminal answer — thoughts, tool work, and
 * intermediate text parts alike — behind a "Worked for X" fold anchored where
 * the hidden work began. This mirrors the desktop transcript's turnFolds:
 * only the trailing run of answer text stays visible. Hidden rows are
 * re-emitted for turn ids present in `expandedFolds`.
 *
 * Linear in messages + blocks + turns: a commit lands at up to ~8 Hz and the
 * largest real sessions carry thousands of messages.
 */
export function buildTranscriptPipeline(
  session: AgentSession,
  expandedFolds: ReadonlySet<string> = new Set(),
): TranscriptPipelineRow[] {
  const runningTurnId = session.turns.find((turn) => turn.status === 'running')?.id ?? null;
  const latestBlock = session.transcript_blocks.at(-1);

  const blocksByAnchor = new Map<number, Array<{ block: TranscriptBlock; index: number }>>();
  session.transcript_blocks.forEach((block, index) => {
    const bucket = blocksByAnchor.get(block.after_message);
    if (bucket) bucket.push({ block, index });
    else blocksByAnchor.set(block.after_message, [{ block, index }]);
  });

  const tagged: TaggedRow[] = [];
  const foldableByTurn = new Map<string, TaggedRow[]>();
  const lastRowByTurn = new Map<string, TaggedRow>();
  const tag = (item: TaggedRow) => {
    tagged.push(item);
    if (!item.turnId) return;
    lastRowByTurn.set(item.turnId, item);
    if (!item.foldable) return;
    const bucket = foldableByTurn.get(item.turnId);
    if (bucket) bucket.push(item);
    else foldableByTurn.set(item.turnId, [item]);
  };
  for (let messageIndex = 0; messageIndex <= session.messages.length; messageIndex += 1) {
    for (const { block, index } of blocksByAnchor.get(messageIndex) ?? []) {
      tag({
        row: {
          kind: 'activities',
          key: `activity:${index}:${block.turn_id ?? 'none'}:${block.after_message}`,
          turnId: block.turn_id,
          block,
          blockIndex: index,
          live: Boolean(
            runningTurnId && block.turn_id === runningTurnId && block === latestBlock &&
              block.after_message === session.messages.length,
          ),
        },
        turnId: block.turn_id,
        foldable: true,
        answerText: false,
      });
    }
    const message = session.messages[messageIndex];
    if (message) {
      tag({
        row: {
          kind: 'message',
          key: `message:${message.id}`,
          turnId: message.turn_id,
          message,
          footerTimestamp: null,
        },
        turnId: message.turn_id,
        foldable: message.role === 'assistant',
        answerText: message.role === 'assistant' && Boolean(message.content.trim()),
      });
    }
  }

  // Per settled turn: which rows hide behind the fold and where it anchors.
  const hidden = new Set<TaggedRow>();
  const anchors = new Map<TaggedRow, AgentTurn>();
  for (const turn of session.turns) {
    if (turn.status === 'running') continue;
    const turnRows = foldableByTurn.get(turn.id);
    if (!turnRows) continue;
    const answerStart = turnAnswerStart(turnRows, (item) => item.answerText);
    const work = turnRows.slice(0, answerStart);
    if (!work.length) continue;
    anchors.set(work[0]!, turn);
    for (const item of work) hidden.add(item);
  }

  const turnsById = new Map(session.turns.map((turn) => [turn.id, turn]));
  const rows: TranscriptPipelineRow[] = [];
  const answerRowByTurn = new Map<string, Extract<TranscriptPipelineRow, { kind: 'message' }>>();
  for (const item of tagged) {
    const anchorTurn = anchors.get(item);
    if (anchorTurn) {
      rows.push({
        kind: 'fold',
        key: `fold:${anchorTurn.id}`,
        turnId: anchorTurn.id,
        turn: anchorTurn,
        label: turnFoldLabel(anchorTurn),
        expanded: expandedFolds.has(anchorTurn.id),
      });
    }
    if (!hidden.has(item)) {
      rows.push(item.row);
      if (item.answerText && item.row.kind === 'message' && item.turnId) {
        answerRowByTurn.set(item.turnId, item.row);
      }
    } else if (item.turnId && expandedFolds.has(item.turnId)) {
      rows.push(item.row);
    }
    if (item.turnId && lastRowByTurn.get(item.turnId) === item) {
      const turn = turnsById.get(item.turnId);
      if (turn && turn.status !== 'running' && turn.checkpoint?.files.length) {
        rows.push({ kind: 'changed', key: `changed:${turn.id}`, turnId: turn.id, checkpoint: turn.checkpoint });
      }
    }
  }
  for (const [turnId, row] of answerRowByTurn) {
    const turn = turnsById.get(turnId);
    if (turn?.completed_at && !row.message.streaming) row.footerTimestamp = turn.completed_at;
  }
  return rows;
}

/** Full expansion — every pipeline row into block rows. Tests and small
 * transcripts; the list expands only its rendered tail. */
export function buildTranscriptRows(
  session: AgentSession,
  expandedFolds: ReadonlySet<string> = new Set(),
  md: TranscriptMarkdownCache = new TranscriptMarkdownCache(),
): TranscriptRow[] {
  return expandTranscriptRows(buildTranscriptPipeline(session, expandedFolds), md, 0);
}

/**
 * Expand pipeline rows from index `start` to the end into render rows, and
 * resolve leading gaps. The gap of the first expanded row is measured against
 * the nearest earlier row so a windowed tail lays out exactly as the full
 * transcript would.
 */
export function expandTranscriptRows(
  pipeline: readonly TranscriptPipelineRow[],
  md: TranscriptMarkdownCache,
  start = 0,
): TranscriptRow[] {
  const from = Math.max(0, Math.min(start, pipeline.length));
  const out: TranscriptRow[] = [];
  const liveIds = new Set<string>();
  for (let ix = from; ix < pipeline.length; ix += 1) {
    expandPipelineRow(pipeline[ix]!, md, liveIds, out);
  }

  // The nearest earlier row sets the first gap (an assistant message with
  // no blocks yields no row, so keep looking back).
  let previous: TranscriptRow | undefined;
  for (let ix = from - 1; ix >= 0 && !previous; ix -= 1) {
    const expanded: TranscriptRow[] = [];
    expandPipelineRow(pipeline[ix]!, md, liveIds, expanded);
    previous = expanded[expanded.length - 1];
  }
  md.prune(liveIds);

  for (const row of out) {
    row.topGap = topGap(row, previous);
    previous = row;
  }
  return out;
}

function expandPipelineRow(
  row: TranscriptPipelineRow,
  md: TranscriptMarkdownCache,
  liveIds: Set<string>,
  out: TranscriptRow[],
) {
  if (row.kind !== 'message') {
    out.push({ ...row, topGap: 0 });
    return;
  }
  const message = row.message;
  if (message.role === 'user') {
    out.push({ kind: 'user', key: `user:${message.id}`, turnId: row.turnId, message, topGap: 0 });
    return;
  }
  if (message.role === 'system') {
    out.push({ kind: 'system', key: `system:${message.id}`, turnId: row.turnId, message, topGap: 0 });
    return;
  }
  liveIds.add(message.id);
  const streaming = message.streaming === true;
  const blocks = md.blocksFor(message.id, message.content, streaming);
  for (let ix = 0; ix < blocks.length; ix += 1) {
    const block = blocks[ix]!;
    const last = ix === blocks.length - 1;
    out.push({
      kind: 'md',
      key: `md:${message.id}.${ix}`,
      turnId: row.turnId,
      messageId: message.id,
      blockIx: ix,
      source: block.source,
      node: block.node,
      live: streaming && last,
      streaming,
      footerTimestamp: last ? row.footerTimestamp : null,
      topGap: 0,
    });
  }
}

function topGap(row: TranscriptRow, previous: TranscriptRow | undefined): number {
  if (!previous) return GAP_FIRST;
  if (row.kind === 'md' && previous.kind === 'md' && previous.messageId === row.messageId) {
    return GAP_BLOCK;
  }
  // Tool stacks, folds, and change cards are denser than prose — one group
  // step on both boundaries, matching the desktop.
  if (row.kind !== 'user' && row.kind !== 'system' && row.kind !== 'md') return GAP_GROUP;
  if (previous.kind !== 'user' && previous.kind !== 'system' && previous.kind !== 'md') {
    return GAP_GROUP;
  }
  return GAP_TURN;
}

/**
 * Reuse the previous render's row objects wherever nothing about a row
 * changed, so memoized row views bail out and a stream commit re-renders only
 * the live tail. Keys are unique per transcript, so the merge is one map
 * lookup per row.
 */
export function stabilizeTranscriptRows(
  previous: readonly TranscriptRow[],
  next: readonly TranscriptRow[],
): TranscriptRow[] {
  if (!previous.length) return [...next];
  const byKey = new Map<string, TranscriptRow>();
  for (const row of previous) byKey.set(row.key, row);
  let changed = previous.length !== next.length;
  const out = next.map((row, index) => {
    const before = byKey.get(row.key);
    if (before && shallowEqualRow(before, row)) {
      if (previous[index] !== before) changed = true;
      return before;
    }
    changed = true;
    return row;
  });
  return changed ? out : (previous as TranscriptRow[]);
}

/** Field-wise equality. Markdown rows compare by `source`, not node
 * identity: the incremental parser re-creates the last two block nodes on
 * every append, and equal source parses to an equivalent tree. */
function shallowEqualRow(a: TranscriptRow, b: TranscriptRow): boolean {
  if (a === b) return true;
  const left = a as unknown as Record<string, unknown>;
  const right = b as unknown as Record<string, unknown>;
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  for (const key of keys) {
    if (key === 'node' && a.kind === 'md') continue;
    if (left[key] !== right[key]) return false;
  }
  return true;
}
