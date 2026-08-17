import type {
  ComposerDraft,
  ComposerDraftChange,
  ComposerDrafts,
  ComposerDraftTarget,
} from '@waku/client'

export function composerDraftId(target: ComposerDraftTarget): string {
  return target.type === 'newSession'
    ? `new:${target.projectId}`
    : `session:${target.sessionId}`
}

export function composerDraftFor(
  drafts: ComposerDrafts,
  target: ComposerDraftTarget,
): ComposerDraft {
  const draft = target.type === 'newSession'
    ? drafts.new_sessions?.[target.projectId]
    : drafts.sessions?.[target.sessionId]
  return {
    text: draft?.text ?? '',
    attachments: draft?.attachments ?? [],
  }
}

export function setComposerDraft(
  drafts: ComposerDrafts,
  target: ComposerDraftTarget,
  draft: ComposerDraft,
): ComposerDraftChange | null {
  const normalized = normalizeDraft(draft)
  const current = composerDraftFor(drafts, target)
  if (draftsEqual(current, normalized)) return null
  const entries = target.type === 'newSession'
    ? (drafts.new_sessions ??= {})
    : (drafts.sessions ??= {})
  const id = target.type === 'newSession' ? target.projectId : target.sessionId
  if (draftEmpty(normalized)) delete entries[id]
  else entries[id] = normalized
  return { target, draft: draftEmpty(normalized) ? null : normalized }
}

export function moveComposerDraftToEmpty(
  drafts: ComposerDrafts,
  source: ComposerDraftTarget,
  destination: ComposerDraftTarget,
): ComposerDraftChange[] {
  if (composerDraftId(source) === composerDraftId(destination)) return []
  const sourceDraft = composerDraftFor(drafts, source)
  if (draftEmpty(sourceDraft) || !draftEmpty(composerDraftFor(drafts, destination))) return []
  const changes: ComposerDraftChange[] = []
  const destinationChange = setComposerDraft(drafts, destination, sourceDraft)
  const sourceChange = setComposerDraft(drafts, source, { text: '', attachments: [] })
  if (destinationChange) changes.push(destinationChange)
  if (sourceChange) changes.push(sourceChange)
  return changes
}

function normalizeDraft(draft: ComposerDraft): ComposerDraft {
  return {
    text: draft.text ?? '',
    attachments: draft.attachments ?? [],
  }
}

function draftEmpty(draft: ComposerDraft): boolean {
  return !(draft.text?.length || draft.attachments?.length)
}

function draftsEqual(left: ComposerDraft, right: ComposerDraft): boolean {
  return left.text === right.text
    && JSON.stringify(left.attachments) === JSON.stringify(right.attachments)
}
