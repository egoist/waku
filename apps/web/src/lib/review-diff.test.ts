import { describe, expect, test } from 'bun:test'
import { hydratePartialDiff, parsePatchFiles } from '@pierre/diffs'
import type { AgentSession, ReviewDiffSource } from '@waku/client'
import {
  compactReviewPatch,
  createReviewDiffLoader,
  latestReviewTurnSource,
  reviewDiffSourceLabel,
  sameReviewDiffSource,
} from './review-diff'

describe('desktop review sources', () => {
  test('uses the newest ready checkpoint as Last turn', () => {
    const session = {
      id: 'session-1',
      turns: [
        turn('turn-1', 1, 'ready'),
        turn('turn-2', 2, 'unavailable'),
        turn('turn-3', 3, 'ready'),
      ],
    } as AgentSession

    expect(latestReviewTurnSource(session)).toEqual(lastTurn('session-1', 'turn-3', 3))
  })

  test('labels an older checkpoint by turn and compares structured sources by value', () => {
    const latest = lastTurn('session-1', 'turn-3', 3)
    const older = lastTurn('session-1', 'turn-1', 1)

    expect(sameReviewDiffSource(latest, lastTurn('session-1', 'turn-3', 3))).toBe(true)
    expect(sameReviewDiffSource(latest, older)).toBe(false)
    expect(reviewDiffSourceLabel(latest, latest)).toBe('Last turn')
    expect(reviewDiffSourceLabel(older, latest)).toBe('Turn 1')
    expect(reviewDiffSourceLabel('staged', latest)).toBe('Staged')
  })
})

describe('compactReviewPatch', () => {
  test('trims hydrated context independently for every file', () => {
    const patch = hydratedFilePatch('one.txt', 20) + hydratedFilePatch('two.txt', 30)

    const compacted = compactReviewPatch(patch)
    const files = parsePatchFiles(compacted, 'test', true).flatMap((entry) => entry.files)

    expect(files).toHaveLength(2)
    expect(compacted).toContain('@@ -17,7 +17,7 @@')
    expect(compacted).toContain('@@ -27,7 +27,7 @@')
    expect(compacted).not.toContain('\n line 1\n')
    expect(compacted).not.toContain('\n line 40\n')
  })

  test('hydrates a compact file when unchanged context is expanded', async () => {
    const patch = hydratedFilePatch('one.txt', 20)
    const compacted = compactReviewPatch(patch)
    const file = parsePatchFiles(compacted, 'compact', true)[0]?.files[0]
    expect(file).toBeDefined()

    const loaded = await createReviewDiffLoader(patch, 'test')(file!)
    const hydrated = hydratePartialDiff('clone', file!, loaded)

    expect(hydrated.isPartial).toBe(false)
    expect(hydrated.deletionLines.join('')).toContain('line 1\n')
    expect(hydrated.additionLines.join('')).toContain('line 40\n')
  })
})

function hydratedFilePatch(name: string, changedLine: number): string {
  const lines = Array.from({ length: 40 }, (_, index) => ` line ${index + 1}`)
  lines[changedLine - 1] = `-line ${changedLine}`
  lines.splice(changedLine, 0, `+changed ${changedLine}`)
  return [
    `diff --git a/${name} b/${name}`,
    `--- a/${name}`,
    `+++ b/${name}`,
    '@@ -1,40 +1,40 @@',
    ...lines,
  ].join('\n') + '\n'
}

function turn(id: string, turnCount: number, status: 'ready' | 'unavailable' | 'error') {
  return {
    id,
    turn_count: turnCount,
    checkpoint: {
      status,
    },
  }
}

function lastTurn(sessionId: string, turnId: string, turnCount: number): ReviewDiffSource {
  return {
    lastTurn: {
      session_id: sessionId,
      turn_id: turnId,
      turn_count: turnCount,
    },
  }
}
