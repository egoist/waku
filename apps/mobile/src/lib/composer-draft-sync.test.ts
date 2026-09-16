import { describe, expect, test } from 'bun:test';

import {
  ComposerDraftSaveTracker,
  ComposerDraftWriteQueue,
} from './composer-draft-sync';

describe('ComposerDraftSaveTracker', () => {
  test('does not persist a draft that was only hydrated from the daemon', () => {
    const tracker = new ComposerDraftSaveTracker();

    expect(tracker.dirty).toBe(false);
    expect(tracker.pendingRevision()).toBeNull();
  });

  test('becomes clean after the latest local edit is saved', () => {
    const tracker = new ComposerDraftSaveTracker();
    tracker.markEdited();

    const revision = tracker.pendingRevision();
    expect(revision).not.toBeNull();
    tracker.markSaved(revision!);

    expect(tracker.dirty).toBe(false);
    expect(tracker.pendingRevision()).toBeNull();
  });

  test('an older save cannot mark a newer edit as synchronized', () => {
    const tracker = new ComposerDraftSaveTracker();
    tracker.markEdited();
    const olderRevision = tracker.pendingRevision();
    tracker.markEdited();

    tracker.markSaved(olderRevision!);

    expect(tracker.dirty).toBe(true);
    expect(tracker.pendingRevision()).not.toBe(olderRevision);
  });

  test('explicit submission clears the local dirty state', () => {
    const tracker = new ComposerDraftSaveTracker();
    tracker.markEdited();

    tracker.markSynchronized();

    expect(tracker.dirty).toBe(false);
  });
});

describe('ComposerDraftWriteQueue', () => {
  test('a submit-time removal waits for the older debounced write', async () => {
    const queue = new ComposerDraftWriteQueue();
    const order: string[] = [];
    let finishDraft!: () => void;
    let noteDraftStarted!: () => void;
    const draftFinished = new Promise<void>((resolve) => {
      finishDraft = resolve;
    });
    const draftStarted = new Promise<void>((resolve) => {
      noteDraftStarted = resolve;
    });

    const draft = queue.enqueue(async () => {
      order.push('draft started');
      noteDraftStarted();
      await draftFinished;
      order.push('draft finished');
    });
    const removal = queue.enqueue(async () => {
      order.push('draft removed');
    });

    await draftStarted;
    expect(order).toEqual(['draft started']);
    finishDraft();
    await Promise.all([draft, removal]);
    expect(order).toEqual(['draft started', 'draft finished', 'draft removed']);
  });

  test('a failed write does not prevent the removal behind it', async () => {
    const queue = new ComposerDraftWriteQueue();
    const failed = queue.enqueue(async () => {
      throw new Error('offline');
    });
    const removal = queue.enqueue(async () => {});

    await expect(failed).rejects.toThrow('offline');
    await expect(removal).resolves.toBeUndefined();
  });
});
