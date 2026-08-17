import {
  parsePatchFiles,
  trimPatchContext,
  type FileContents,
  type FileDiffContentsLoader,
  type FileDiffMetadata,
} from '@pierre/diffs'
import type { AgentSession, ReviewDiffSource } from '@waku/client'

const REVIEW_DIFF_CONTEXT_LINES = 3

export function latestReviewTurnSource(session: AgentSession | null): ReviewDiffSource | null {
  if (!session) return null
  let turn: AgentSession['turns'][number] | undefined
  for (let index = session.turns.length - 1; index >= 0; index -= 1) {
    const candidate = session.turns[index]!
    if (candidate.turn_count > 0 && candidate.checkpoint?.status === 'ready') {
      turn = candidate
      break
    }
  }
  return turn
    ? {
        lastTurn: {
          session_id: session.id,
          turn_id: turn.id,
          turn_count: turn.turn_count,
        },
      }
    : null
}

export function sameReviewDiffSource(
  left: ReviewDiffSource,
  right: ReviewDiffSource,
): boolean {
  if (typeof left === 'string' || typeof right === 'string') return left === right
  return left.lastTurn.session_id === right.lastTurn.session_id
    && left.lastTurn.turn_id === right.lastTurn.turn_id
    && left.lastTurn.turn_count === right.lastTurn.turn_count
}

export function reviewDiffSourceLabel(
  source: ReviewDiffSource,
  latest: ReviewDiffSource | null,
  t?: (key: string, params?: Record<string, string | number>) => string,
): string {
  if (typeof source === 'object') {
    return latest && sameReviewDiffSource(source, latest)
      ? t ? t('diff.source_last_turn') : 'Last turn'
      : t ? t('diff.source_turn', { turn: source.lastTurn.turn_count }) : `Turn ${source.lastTurn.turn_count}`
  }
  if (t) return t({
    uncommitted: 'diff.source_uncommitted',
    unstaged: 'diff.source_unstaged',
    staged: 'diff.source_staged',
    committed: 'diff.source_committed',
    branch: 'diff.source_branch',
  }[source])
  return {
    uncommitted: 'Uncommitted',
    unstaged: 'Unstaged',
    staged: 'Staged',
    committed: 'Committed',
    branch: 'Branch',
  }[source]
}

export function compactReviewPatch(patch: string): string {
  // trimPatchContext handles one unified file patch at a time. Feeding it a
  // multi-file Git patch makes the following `diff --git` header look like
  // hunk contents, which produces invalid line counts.
  return patch
    .split(/(?=^diff --git )/m)
    .map((filePatch) => trimPatchContext(filePatch, REVIEW_DIFF_CONTEXT_LINES))
    .join('')
}

export function createReviewDiffLoader(
  patch: string,
  cacheKeyPrefix: string,
): FileDiffContentsLoader {
  let hydratedFiles: Map<string, FileDiffMetadata> | undefined

  return async (fileDiff) => {
    hydratedFiles ??= new Map(
      parsePatchFiles(patch, `${cacheKeyPrefix}:full`, true)
        .flatMap((entry) => entry.files)
        .map((file) => [fileIdentity(file), file]),
    )
    const hydrated = hydratedFiles.get(fileIdentity(fileDiff))
    if (!hydrated) throw new Error(`Could not hydrate diff context for ${fileDiff.name}`)

    const newFile = fileContents(hydrated, 'new')
    if (hydrated.type === 'rename-pure') return { oldFile: null, newFile }
    return {
      oldFile: fileContents(hydrated, 'old'),
      newFile,
    }
  }
}

function fileIdentity(file: FileDiffMetadata): string {
  return `${file.prevName ?? ''}\0${file.name}\0${file.type}`
}

function fileContents(file: FileDiffMetadata, side: 'old' | 'new'): FileContents {
  return {
    name: side === 'old' ? file.prevName ?? file.name : file.name,
    contents: (side === 'old' ? file.deletionLines : file.additionLines).join(''),
    cacheKey: file.cacheKey ? `${file.cacheKey}:${side}` : undefined,
  }
}
