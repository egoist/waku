import { describe, expect, test } from 'bun:test';
import type { AgentSession, AgentTurn, Project } from '@waku/client';
import { activitiesForBlock } from '@waku/client/event-reducer';

import { TranscriptMarkdownCache } from '../md/transcript-cache';
import {
  buildTranscriptPipeline,
  buildTranscriptRows,
  contextPercent,
  displaySessionTitle,
  expandTranscriptRows,
  findActivityBlock,
  groupSessions,
  relativeSessionTime,
  sessionDateGroup,
  stabilizeTranscriptRows,
} from './session-presentation';

describe('mobile session presentation', () => {
  test('uses provider title for untouched tasks', () => {
    expect(displaySessionTitle(session({ title: 'New task', auto_title: 'Fix login' }))).toBe(
      'Fix login',
    );
  });

  test('groups started sessions by the desktop calendar periods, newest first', () => {
    const now = new Date(2026, 7, 31, 12);
    const projects: Project[] = [{ id: 'project', name: 'Waku', path: '/waku', created_at: 1 }];
    const current = session({ id: 'new', last_reply_at: epoch(2026, 7, 31, 11) });
    const yesterday = session({ id: 'old', last_reply_at: epoch(2026, 7, 30, 20) });
    const earlier = session({ id: 'earlier', last_reply_at: epoch(2026, 7, 20, 20) });
    const empty = session({ id: 'empty', last_reply_at: null, messages: [], turns: [] });
    expect(groupSessions(projects, [earlier, yesterday, empty, current], now).map((group) => ({
      id: group.id,
      sessions: group.data.map((item) => item.session.id),
    }))).toEqual([
      { id: 'today', sessions: ['new'] },
      { id: 'yesterday', sessions: ['old'] },
      { id: 'month', sessions: ['earlier'] },
    ]);
  });

  test('formats compact recency labels', () => {
    expect(relativeSessionTime(1_000, 1_030_000)).toBe('just now');
    expect(relativeSessionTime(1_000, 1_300_000)).toBe('5m');
    const now = new Date(2026, 7, 12, 12);
    expect(sessionDateGroup(epoch(2026, 7, 12, 12), now)).toBe('today');
    expect(sessionDateGroup(epoch(2026, 7, 11, 12), now)).toBe('yesterday');
    expect(sessionDateGroup(epoch(2026, 7, 10, 12), now)).toBe('week');
    expect(sessionDateGroup(epoch(2026, 7, 1, 12), now)).toBe('month');
    expect(sessionDateGroup(epoch(2026, 0, 1, 12), now)).toBe('year');
    expect(sessionDateGroup(epoch(2025, 11, 31, 12), now)).toBe('more');
  });

  test('reports context usage as a bounded percentage', () => {
    expect(contextPercent(session({}))).toBeNull();
    expect(contextPercent(session({ context_usage: { tokens: 50_000, window: 200_000 } }))).toBe(25);
    expect(contextPercent(session({ context_usage: { tokens: 500, window: null } }))).toBeNull();
  });

  test('keeps provider ordering inline when the turn is unknown', () => {
    const current = session({
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
        { id: 'agent', turn_id: 'turn', role: 'assistant', content: 'done', created_at: 2, streaming: false },
      ],
      transcript_blocks: [activityBlock(1, 'turn')],
    });
    expect(buildTranscriptRows(current).map((row) => row.kind)).toEqual([
      'user',
      'activities',
      'md',
    ]);
  });

  test('folds a settled turn behind “Worked for X” like the desktop', () => {
    const current = session({
      turns: [turn({ id: 'turn', status: 'completed', started_at: 10, completed_at: 130 })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
        { id: 'agent', turn_id: 'turn', role: 'assistant', content: 'done', created_at: 2, streaming: false },
      ],
      transcript_blocks: [activityBlock(1, 'turn')],
    });
    const collapsed = buildTranscriptRows(current);
    expect(collapsed.map((row) => row.kind)).toEqual(['user', 'fold', 'md']);
    const fold = collapsed[1]!;
    if (fold.kind !== 'fold') throw new Error('expected fold');
    expect(fold.label).toBe('Worked for 2 minutes');
    const answer = collapsed[2]!;
    if (answer.kind !== 'md') throw new Error('expected md');
    expect(answer.footerTimestamp).toBe(130);

    const expanded = buildTranscriptRows(current, new Set(['turn']));
    expect(expanded.map((row) => row.kind)).toEqual(['user', 'fold', 'activities', 'md']);
  });

  test('folds thoughts too — a thought-only turn shows just the answer', () => {
    const current = session({
      turns: [turn({ id: 'turn', status: 'completed', started_at: 10, completed_at: 15 })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'hi', created_at: 1, streaming: false },
        { id: 'agent', turn_id: 'turn', role: 'assistant', content: 'Hi!', created_at: 2, streaming: false },
      ],
      transcript_blocks: [{
        after_message: 1,
        turn_id: 'turn',
        content: {
          kind: 'activities',
          data: [{
            id: 'thought',
            source_id: null,
            kind: 'reasoning',
            title: 'Reasoning',
            detail: null,
            failed: false,
            complete: true,
            reasoning: { content: 'Preparing greeting', started_at_ms: 0, finished_at_ms: 900 },
          }],
        },
      }],
    });
    expect(buildTranscriptRows(current).map((row) => row.kind)).toEqual([
      'user',
      'fold',
      'md',
    ]);
  });

  test('hides intermediate text parts — only the terminal answer stays visible', () => {
    const current = session({
      turns: [turn({ id: 'turn', status: 'completed', started_at: 10, completed_at: 40 })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
        { id: 'part1', turn_id: 'turn', role: 'assistant', content: 'First part.', created_at: 2, streaming: false },
        { id: 'part2', turn_id: 'turn', role: 'assistant', content: 'Final answer.', created_at: 3, streaming: false },
      ],
      transcript_blocks: [activityBlock(2, 'turn')],
    });
    const collapsed = buildTranscriptRows(current);
    expect(collapsed.map((row) => (
      row.kind === 'md' ? `md:${row.messageId}` : row.kind
    ))).toEqual(['user', 'fold', 'md:part2']);
    const answer = collapsed[2]!;
    if (answer.kind !== 'md') throw new Error('expected md');
    expect(answer.footerTimestamp).toBe(40);

    const expanded = buildTranscriptRows(current, new Set(['turn']));
    expect(expanded.map((row) => (
      row.kind === 'md' ? `md:${row.messageId}` : row.kind
    ))).toEqual(['user', 'fold', 'md:part1', 'activities', 'md:part2']);
  });

  test('keeps a running turn’s work expanded and live', () => {
    const current = session({
      status: 'working',
      turns: [turn({ id: 'turn', status: 'running', started_at: 10, completed_at: null })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
      ],
      transcript_blocks: [activityBlock(1, 'turn')],
    });
    const rows = buildTranscriptRows(current);
    expect(rows.map((row) => row.kind)).toEqual(['user', 'activities']);
    const activities = rows[1]!;
    if (activities.kind !== 'activities') throw new Error('expected activities');
    expect(activities.live).toBe(true);
  });

  test('emits a changed-files card after a checkpointed turn', () => {
    const current = session({
      turns: [turn({
        id: 'turn',
        status: 'completed',
        started_at: 10,
        completed_at: 70,
        checkpoint: {
          turn_count: 1,
          git_ref: 'refs/waku/x',
          status: 'ready',
          files: [{ path: 'src/a.ts', additions: 3, deletions: 1 }],
          additions: 3,
          deletions: 1,
          created_at: 70,
        },
      })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
        { id: 'agent', turn_id: 'turn', role: 'assistant', content: 'done', created_at: 2, streaming: false },
      ],
      transcript_blocks: [],
    });
    expect(buildTranscriptRows(current).map((row) => row.kind)).toEqual([
      'user',
      'md',
      'changed',
    ]);
  });

  test('splits assistant messages into block rows with the spacing tokens', () => {
    const current = session({
      status: 'working',
      turns: [turn({ id: 'turn', status: 'running', started_at: 10, completed_at: null })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
        {
          id: 'agent',
          turn_id: 'turn',
          role: 'assistant',
          content: '# Title\n\nFirst paragraph.\n\nSecond paragraph grows',
          created_at: 2,
          streaming: true,
        },
      ],
    });
    const rows = buildTranscriptRows(current);
    expect(rows.map((row) => row.kind)).toEqual(['user', 'md', 'md', 'md']);
    expect(rows.map((row) => row.kind === 'md' ? row.live : null)).toEqual([
      null, false, false, true,
    ]);
    expect(rows.map((row) => row.topGap)).toEqual([26, 16, 12, 12]);
    expect(rows[1]!.key).toBe('md:agent.0');
    expect(rows[3]!.key).toBe('md:agent.2');
  });

  test('a windowed tail lays out exactly like the full transcript', () => {
    const current = session({
      turns: [
        turn({ id: 't1', status: 'completed', started_at: 10, completed_at: 20 }),
        turn({ id: 't2', status: 'completed', started_at: 30, completed_at: 40 }),
      ],
      messages: [
        { id: 'u1', turn_id: 't1', role: 'user', content: 'one', created_at: 1, streaming: false },
        { id: 'a1', turn_id: 't1', role: 'assistant', content: 'First.\n\nSecond.', created_at: 2, streaming: false },
        { id: 'u2', turn_id: 't2', role: 'user', content: 'two', created_at: 3, streaming: false },
        { id: 'a2', turn_id: 't2', role: 'assistant', content: 'Third.', created_at: 4, streaming: false },
      ],
      transcript_blocks: [activityBlock(1, 't1'), activityBlock(3, 't2')],
    });
    const md = new TranscriptMarkdownCache();
    const pipeline = buildTranscriptPipeline(current);
    expect(pipeline.map((row) => row.kind)).toEqual([
      'message', 'fold', 'message', 'message', 'fold', 'message',
    ]);
    const full = expandTranscriptRows(pipeline, md, 0);
    const tail = expandTranscriptRows(pipeline, md, 3);
    expect(tail.map((row) => row.key)).toEqual(full.slice(-3).map((row) => row.key));
    expect(tail.map((row) => row.topGap)).toEqual(full.slice(-3).map((row) => row.topGap));
    // The first mounted row measures its gap against the unmounted row
    // before it — here the fold above the first answer.
    const fromAnswer = expandTranscriptRows(pipeline, md, 2);
    expect(fromAnswer.map((row) => row.key).slice(0, 2)).toEqual(['md:a1.0', 'md:a1.1']);
    expect(fromAnswer.map((row) => row.topGap).slice(0, 2)).toEqual([12, 12]);
  });

  test('stabilizes row identity across commits so memoized rows bail out', () => {
    const md = new TranscriptMarkdownCache();
    const before = buildTranscriptRows(session({
      status: 'working',
      turns: [turn({ id: 'turn', status: 'running', started_at: 10, completed_at: null })],
      messages: [
        { id: 'user', turn_id: 'turn', role: 'user', content: 'go', created_at: 1, streaming: false },
        { id: 'agent', turn_id: 'turn', role: 'assistant', content: 'One.\n\nTwo', created_at: 2, streaming: true },
      ],
    }), new Set(), md);
    const after = buildTranscriptRows(session({
      status: 'working',
      turns: [turn({ id: 'turn', status: 'running', started_at: 10, completed_at: null })],
      messages: [
        before[0]!.kind === 'user' ? before[0].message : (() => { throw new Error('user'); })(),
        { id: 'agent', turn_id: 'turn', role: 'assistant', content: 'One.\n\nTwo more', created_at: 2, streaming: true },
      ],
    }), new Set(), md);
    const stable = stabilizeTranscriptRows(before, after);
    expect(stable[0]).toBe(before[0]!);
    expect(stable[1]).toBe(before[1]!);
    expect(stable[2]).not.toBe(before[2]!);
    expect(stable[2]!.kind === 'md' && stable[2].source).toBe('Two more');
    expect(stabilizeTranscriptRows(stable, stabilizeTranscriptRows(stable, after))).toBe(stable);
  });
});

describe('activity sheet locator', () => {
  test('trusts the index hint only while the anchor still matches', () => {
    const first = activityBlock(0, 'turn');
    const second = activityBlock(1, 'turn');
    const current = session({ transcript_blocks: [first, second] });
    expect(findActivityBlock(current, { blockIndex: 1, turnId: 'turn', afterMessage: 1 })).toBe(second);
    // A rewind dropped a block in front: the hint is stale, the anchor is not.
    expect(findActivityBlock(current, { blockIndex: 1, turnId: 'turn', afterMessage: 0 })).toBe(first);
    expect(findActivityBlock(current, { blockIndex: 0, turnId: 'other', afterMessage: 0 })).toBeNull();
  });

  test('survives the per-commit clone by resolving against the new session', () => {
    const before = session({ transcript_blocks: [activityBlock(0, 'turn')] });
    const after = JSON.parse(JSON.stringify(before)) as AgentSession;
    activitiesForBlock(after.transcript_blocks[0]!)[0]!.output = 'streamed';
    const target = { blockIndex: 0, turnId: 'turn', afterMessage: 0 };
    expect(activitiesForBlock(findActivityBlock(before, target)!)[0]!.output).toBeUndefined();
    expect(activitiesForBlock(findActivityBlock(after, target)!)[0]!.output).toBe('streamed');
  });
});

function activityBlock(afterMessage: number, turnId: string): AgentSession['transcript_blocks'][number] {
  return {
    after_message: afterMessage,
    turn_id: turnId,
    content: {
      kind: 'activities',
      data: [{
        id: 'tool',
        source_id: null,
        kind: 'command',
        title: 'Run tests',
        detail: null,
        failed: false,
        complete: true,
      }],
    },
  };
}

function turn(overrides: Partial<AgentTurn> & Pick<AgentTurn, 'id' | 'status'>): AgentTurn {
  return {
    turn_count: 1,
    provider_turn_started: true,
    provider_resume_at: null,
    started_at: 1,
    completed_at: 2,
    checkpoint: null,
    ...overrides,
  };
}

function session(overrides: Partial<AgentSession>): AgentSession {
  return {
    id: 'session',
    title: 'Task',
    project_id: 'project',
    provider: 'codex',
    runtime_mode: 'autoAcceptEdits',
    status: 'idle',
    created_at: 1,
    updated_at: 1,
    last_reply_at: 1,
    provider_cursor: null,
    messages: [{
      id: 'message',
      turn_id: null,
      role: 'user',
      content: 'hello',
      created_at: 1,
      streaming: false,
    }],
    transcript_blocks: [],
    turns: [],
    ...overrides,
  };
}

function epoch(year: number, month: number, day: number, hour: number) {
  return Math.floor(new Date(year, month, day, hour).getTime() / 1_000);
}
