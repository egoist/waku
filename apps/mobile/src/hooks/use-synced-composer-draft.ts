import type {
  ComposerDraft,
  ComposerDraftAttachment,
  ComposerDrafts,
  ComposerDraftTarget,
} from '@waku/client';
import { useCallback, useEffect, useRef, useState } from 'react';

import { useComposerDraftRefreshRevision } from './use-composer-draft-refresh';
import {
  ComposerDraftSaveTracker,
  ComposerDraftWriteQueue,
} from '@/lib/composer-draft-sync';
import { applyComposerDraftChanges, loadComposerDrafts } from '@/lib/daemon-api';
import { useDaemon } from '@/lib/daemon-context';

export interface SynchronizedComposerDraft {
  text: string;
  attachments: ComposerDraftAttachment[];
}

interface SyncedComposerDraftOptions {
  target: ComposerDraftTarget | null;
  text: string;
  attachments?: ComposerDraftAttachment[];
  onHydrate: (draft: SynchronizedComposerDraft) => void;
  /** Preserve the visible New Task prompt when its project changes. */
  carryAcrossTargets?: boolean;
  /** Existing-session composers stay mounted across navigation less reliably,
   * so flush a real local edit when their component is destroyed. */
  flushOnUnmount?: boolean;
}

interface SyncedComposerDraft {
  markEdited: () => void;
  removeSubmittedDraft: () => void;
}

/**
 * Synchronizes one mobile composer with the daemon without treating hydration
 * as an edit. Clean drafts refresh on screen/app activation; unsaved local
 * edits win until written, and all writes from this composer remain ordered.
 */
export function useSyncedComposerDraft({
  target,
  text,
  attachments,
  onHydrate,
  carryAcrossTargets = false,
  flushOnUnmount = false,
}: SyncedComposerDraftOptions): SyncedComposerDraft {
  const daemon = useDaemon();
  const refreshRevision = useComposerDraftRefreshRevision();
  const [hydratedTarget, setHydratedTarget] = useState<string | null>(null);
  const lastHydratedTarget = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const trackerRef = useRef<ComposerDraftSaveTracker | null>(null);
  if (!trackerRef.current) trackerRef.current = new ComposerDraftSaveTracker();
  const tracker = trackerRef.current;
  const queueRef = useRef<ComposerDraftWriteQueue | null>(null);
  if (!queueRef.current) queueRef.current = new ComposerDraftWriteQueue();
  const queue = queueRef.current;

  const targetKey = composerDraftTargetKey(daemon.activeProfile?.id ?? null, target);
  const targetRef = useRef(target);
  targetRef.current = target;
  const clientRef = useRef(daemon.client);
  clientRef.current = daemon.client;
  const valueRef = useRef<SynchronizedComposerDraft>({ text, attachments: attachments ?? [] });
  valueRef.current = { text, attachments: attachments ?? [] };
  const hydrateRef = useRef(onHydrate);
  hydrateRef.current = onHydrate;

  useEffect(() => {
    const client = daemon.client;
    const activeTarget = targetRef.current;
    if (
      refreshRevision === null
      || !client
      || daemon.phase !== 'connected'
      || !targetKey
      || !activeTarget
    ) return;

    const authoritative = lastHydratedTarget.current === targetKey;
    if (
      carryAcrossTargets
      && lastHydratedTarget.current
      && lastHydratedTarget.current !== targetKey
      && hasDraftContent(valueRef.current)
      && !tracker.dirty
    ) {
      tracker.markEdited();
    }
    const cleanAtStart = !tracker.dirty;
    setHydratedTarget(null);
    let cancelled = false;
    void loadComposerDrafts(client)
      .then((drafts) => {
        if (cancelled) return;
        if (
          cleanAtStart
          && !tracker.dirty
          && (authoritative || !hasDraftContent(valueRef.current))
        ) {
          const synchronized = normalizeDraft(draftForTarget(drafts, activeTarget));
          valueRef.current = synchronized;
          hydrateRef.current(synchronized);
        }
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) {
          lastHydratedTarget.current = targetKey;
          setHydratedTarget(targetKey);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [
    carryAcrossTargets,
    daemon.client,
    daemon.phase,
    refreshRevision,
    targetKey,
    tracker,
  ]);

  useEffect(() => {
    const client = daemon.client;
    const activeTarget = targetRef.current;
    const saveRevision = tracker.pendingRevision();
    if (
      !client
      || daemon.phase !== 'connected'
      || !targetKey
      || !activeTarget
      || hydratedTarget !== targetKey
      || saveRevision === null
    ) return;

    if (timer.current) clearTimeout(timer.current);
    const snapshot = valueRef.current;
    const pendingTimer = setTimeout(() => {
      if (timer.current === pendingTimer) timer.current = null;
      void queue
        .enqueue(() => applyComposerDraftChanges(client, [{
          target: activeTarget,
          draft: draftPayload(snapshot),
        }]))
        .then(() => {
          tracker.markSaved(saveRevision);
        })
        .catch(() => {});
    }, 800);
    timer.current = pendingTimer;
    return () => {
      clearTimeout(pendingTimer);
      if (timer.current === pendingTimer) timer.current = null;
    };
  }, [
    attachments,
    daemon.client,
    daemon.phase,
    hydratedTarget,
    queue,
    targetKey,
    text,
    tracker,
  ]);

  useEffect(() => () => {
    if (!flushOnUnmount) return;
    if (timer.current) clearTimeout(timer.current);
    const client = clientRef.current;
    const activeTarget = targetRef.current;
    const saveRevision = tracker.pendingRevision();
    if (!client || !activeTarget || saveRevision === null) return;
    const snapshot = valueRef.current;
    void queue
      .enqueue(() => applyComposerDraftChanges(client, [{
        target: activeTarget,
        draft: draftPayload(snapshot),
      }]))
      .then(() => {
        tracker.markSaved(saveRevision);
      })
      .catch(() => {});
  }, [flushOnUnmount, queue, tracker]);

  const markEdited = useCallback(() => {
    tracker.markEdited();
  }, [tracker]);

  const removeSubmittedDraft = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    tracker.markSynchronized();
    const client = clientRef.current;
    const activeTarget = targetRef.current;
    if (!client || !activeTarget) return;
    void queue
      .enqueue(() => applyComposerDraftChanges(client, [{
        target: activeTarget,
        draft: null,
      }]))
      .catch(() => {});
  }, [queue, tracker]);

  return { markEdited, removeSubmittedDraft };
}

function composerDraftTargetKey(
  profileId: string | null,
  target: ComposerDraftTarget | null,
): string | null {
  if (!target) return null;
  const owner = profileId ?? 'daemon';
  return target.type === 'session'
    ? `${owner}:session:${target.sessionId}`
    : `${owner}:newSession:${target.projectId}`;
}

function draftForTarget(
  drafts: ComposerDrafts,
  target: ComposerDraftTarget,
): ComposerDraft | undefined {
  return target.type === 'session'
    ? drafts.sessions?.[target.sessionId]
    : drafts.new_sessions?.[target.projectId];
}

function normalizeDraft(draft: ComposerDraft | undefined): SynchronizedComposerDraft {
  return {
    text: draft?.text ?? '',
    attachments: draft?.attachments ?? [],
  };
}

function hasDraftContent(draft: SynchronizedComposerDraft): boolean {
  return Boolean(draft.text.trim() || draft.attachments.length);
}

function draftPayload(draft: SynchronizedComposerDraft): ComposerDraft | null {
  if (!hasDraftContent(draft)) return null;
  return {
    text: draft.text,
    ...(draft.attachments.length ? { attachments: draft.attachments } : {}),
  };
}
