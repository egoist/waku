/**
 * Tracks whether the visible composer has a local edit that still needs to be
 * written to the daemon. Hydrating a daemon draft never touches this tracker,
 * which prevents a read-only mobile client from echoing stale text back after
 * another client submits and removes it.
 */
export class ComposerDraftSaveTracker {
  private editRevision = 0;
  private savedRevision = 0;

  get dirty(): boolean {
    return this.editRevision !== this.savedRevision;
  }

  markEdited(): void {
    this.editRevision += 1;
  }

  pendingRevision(): number | null {
    return this.dirty ? this.editRevision : null;
  }

  markSaved(revision: number): void {
    if (revision === this.editRevision) this.savedRevision = revision;
  }

  markSynchronized(): void {
    this.savedRevision = this.editRevision;
  }
}

/**
 * Serializes writes from one composer so its submit-time removal cannot be
 * overtaken by an older debounced save on the daemon's request workers.
 */
export class ComposerDraftWriteQueue {
  private tail: Promise<void> = Promise.resolve();

  enqueue(write: () => Promise<void>): Promise<void> {
    const pending = this.tail.catch(() => {}).then(write);
    this.tail = pending;
    return pending;
  }
}
