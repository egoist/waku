import { describe, expect, test } from 'bun:test'
import type { ComposerDrafts } from '@waku/client'
import {
  composerDraftFor,
  moveComposerDraftToEmpty,
  setComposerDraft,
} from './composer-drafts'

describe('composer drafts', () => {
  test('updates one key without replacing unrelated client drafts', () => {
    const drafts: ComposerDrafts = {
      sessions: { other: { text: 'from desktop' } },
    }
    const change = setComposerDraft(
      drafts,
      { type: 'newSession', projectId: 'waku' },
      { text: 'from web' },
    )

    expect(change).toEqual({
      target: { type: 'newSession', projectId: 'waku' },
      draft: { text: 'from web', attachments: [] },
    })
    expect(drafts.sessions?.other?.text).toBe('from desktop')
  })

  test('moves a new-task draft only when the destination is empty', () => {
    const drafts: ComposerDrafts = {
      new_sessions: {
        first: { text: 'carry me' },
        occupied: { text: 'keep me' },
      },
    }
    expect(moveComposerDraftToEmpty(
      drafts,
      { type: 'newSession', projectId: 'first' },
      { type: 'newSession', projectId: 'second' },
    )).toHaveLength(2)
    expect(composerDraftFor(drafts, { type: 'newSession', projectId: 'second' }).text)
      .toBe('carry me')

    expect(moveComposerDraftToEmpty(
      drafts,
      { type: 'newSession', projectId: 'second' },
      { type: 'newSession', projectId: 'occupied' },
    )).toEqual([])
    expect(composerDraftFor(drafts, { type: 'newSession', projectId: 'occupied' }).text)
      .toBe('keep me')
  })
})
