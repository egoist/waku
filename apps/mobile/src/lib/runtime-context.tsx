import { useQueryClient } from '@tanstack/react-query';
import type {
  AgentSession,
  GoalOperation,
  MessageAttachment,
  PendingPermission,
  PendingUserInput,
  ProviderKind,
  SequencedEvent,
  UserInputAnswer,
} from '@waku/client';
import { reduceRuntimeEvent } from '@waku/client/event-reducer';
import { writeProviderProbeCache } from '@waku/client/provider-probe-cache';
import * as Crypto from 'expo-crypto';
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';

import {
  attachDaemonSession,
  daemonKeys,
  hydrateSession,
  loadDaemonSettings,
  loadTaskState,
  materializeWorktree,
  persistSession,
  probeProvider,
  removeDaemonSession,
  type TaskState,
} from './daemon-api';
import { persistentStorageSync } from './composer-preferences-store';
import { useDaemon } from './daemon-context';
import {
  applySessionOptions,
  beginTurn,
  createSession,
  providerPromptForSubmission,
  queueSubmission,
  sessionBusy,
  sessionCwd,
  sessionIsRunning,
  shouldApplyRuntimeEvent,
  submittedTurnIdentity,
  type NewSessionOptions,
  type SessionOptionChanges,
} from './mobile-runtime';
import {
  clearDaemonDisconnectErrors,
  isDaemonDisconnectError,
} from './runtime-errors';

export interface MobileRuntime {
  runtimeId: string;
  supportsSteer: boolean;
  starting: boolean;
  running: boolean;
}

interface RuntimeEntry extends MobileRuntime {
  /** Latest full projection, retained while subscribed even if the screen's
   * query is evicted. Never fall back to the snapshot captured on attach. */
  session: AgentSession;
  lastDriverError: string | null;
  unsubscribe: () => void;
  /** Buffered runtime events awaiting the next commit. */
  pending: SequencedEvent[];
  flushTimer: ReturnType<typeof setTimeout> | null;
  lastFlushAt: number;
}

interface PendingSubmission {
  providerPrompt: string;
  displayContent: string;
  attachments: MessageAttachment[];
}

/** Stream deltas commit at ≤ ~8.3 Hz, the desktop stream pump's cadence: the
 * transcript re-renders per commit, not per provider chunk. Interactive
 * events (permissions, turn lifecycle) flush the buffer immediately. */
const STREAM_COMMIT_MS = 120;

function deferrableEvent(event: SequencedEvent): boolean {
  const kind = event.event.kind;
  return kind === 'textDelta' || kind === 'reasoningDelta' || kind === 'usageUpdated';
}

interface RuntimeContextValue {
  runtimes: Record<string, MobileRuntime | undefined>;
  permissions: Record<string, PendingPermission | undefined>;
  userInputs: Record<string, PendingUserInput | undefined>;
  errors: Record<string, string | undefined>;
  attachSession: (session: AgentSession) => Promise<boolean>;
  sendPrompt: (
    session: AgentSession,
    prompt: string,
    attachments?: MessageAttachment[],
    providerPromptOverride?: string,
  ) => Promise<AgentSession>;
  steerPrompt: (
    session: AgentSession,
    prompt: string,
    attachments?: MessageAttachment[],
    providerPromptOverride?: string,
  ) => Promise<void>;
  createTask: (
    projectId: string,
    provider: ProviderKind,
    isolated: boolean,
    prompt: string,
    options?: NewSessionOptions,
    providerPromptOverride?: string,
  ) => Promise<AgentSession>;
  cancel: (sessionId: string) => Promise<void>;
  respond: (sessionId: string, requestId: string, optionId: string) => Promise<void>;
  respondUserInput: (
    sessionId: string,
    requestId: string,
    answers: UserInputAnswer[],
  ) => Promise<void>;
  updateSessionOptions: (sessionId: string, changes: SessionOptionChanges) => Promise<void>;
  sendGoalOperation: (session: AgentSession, operation: GoalOperation) => Promise<void>;
  renameSession: (sessionId: string, title: string) => Promise<void>;
  deleteSession: (sessionId: string) => Promise<void>;
  removeQueuedMessage: (sessionId: string, messageId: string) => Promise<void>;
  dismissError: (sessionId: string) => void;
}

const RuntimeContext = createContext<RuntimeContextValue | null>(null);
const clock = {
  nowSeconds: () => Math.floor(Date.now() / 1_000),
  nowMillis: () => Date.now(),
  randomUUID: Crypto.randomUUID,
};

export function RuntimeProvider({ children }: { children: ReactNode }) {
  const daemon = useDaemon();
  const queryClient = useQueryClient();
  const [runtimes, setRuntimes] = useState<Record<string, MobileRuntime | undefined>>({});
  const [permissions, setPermissions] = useState<Record<string, PendingPermission | undefined>>({});
  const [userInputs, setUserInputs] = useState<Record<string, PendingUserInput | undefined>>({});
  const [errors, setErrors] = useState<Record<string, string | undefined>>({});
  const hasRecoveredDisconnectError = daemon.phase === 'connected'
    && Object.values(errors).some(isDaemonDisconnectError);
  const entries = useRef(new Map<string, RuntimeEntry>());
  const attachRequests = useRef(new Map<string, Promise<boolean>>());
  /** The daemon connection count whose runtimes were last revalidated. */
  const revalidatedConnections = useRef(0);
  const persistTails = useRef(new Map<string, Promise<AgentSession>>());
  const persistTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  /** Advanced on every cache write of new local state, so a save reply that
   * lands after newer runtime events cannot roll the cache back to the
   * snapshot it saved. */
  const cacheGenerations = useRef(new Map<string, number>());
  const pendingSteers = useRef(new Map<string, PendingSubmission[]>());
  const drainingQueues = useRef(new Set<string>());
  const sendPromptRef = useRef<
    ((
      session: AgentSession,
      prompt: string,
      attachments?: MessageAttachment[],
      providerPromptOverride?: string,
    ) => Promise<AgentSession>) | null
  >(null);

  /** Writes a session into the query cache without advancing its
   * generation: the daemon's echo of a snapshot this client already holds. */
  const writeSessionCache = useCallback((session: AgentSession) => {
    const profileId = daemon.activeProfile?.id;
    if (!profileId) return;
    const entry = entries.current.get(session.id);
    if (entry) entry.session = session;
    queryClient.setQueryData(daemonKeys.session(profileId, session.id), session);
    queryClient.setQueryData<TaskState>(daemonKeys.taskState(profileId), (current) => {
      if (!current) return current;
      const sessions = current.sessions.some((item) => item.id === session.id)
        ? current.sessions.map((item) => item.id === session.id ? session : item)
        : [...current.sessions, session];
      return { ...current, sessions };
    });
  }, [daemon.activeProfile?.id, queryClient]);

  /** Caches new local state — a submission, a runtime event, an edit. */
  const cacheSession = useCallback((session: AgentSession) => {
    cacheGenerations.current.set(
      session.id,
      (cacheGenerations.current.get(session.id) ?? 0) + 1,
    );
    const entry = entries.current.get(session.id);
    const running = sessionIsRunning(session);
    if (entry && entry.running !== running) {
      entry.running = running;
      setRuntimes((current) => ({ ...current, [session.id]: publicRuntime(entry) }));
    }
    writeSessionCache(session);
  }, [writeSessionCache]);

  /** Full session from the query cache, hydrating from the daemon when only
   * the list skeleton is known. A skeleton must never be persisted back — it
   * would blank the stored transcript. */
  const loadFullSession = useCallback(async (sessionId: string): Promise<AgentSession> => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId) throw new Error('Waku daemon is disconnected');
    const cached = queryClient.getQueryData<AgentSession>(
      daemonKeys.session(profileId, sessionId),
    ) ?? entries.current.get(sessionId)?.session;
    if (cached) return cached;
    // Share the screen's in-flight hydration. An independent fetch could
    // finish after subscription and overwrite events already applied live.
    const hydrated = await queryClient.fetchQuery({
      queryKey: daemonKeys.session(profileId, sessionId),
      queryFn: () => hydrateSession(client, sessionId),
    });
    if (!hydrated) throw new Error('This task no longer exists on the daemon');
    return hydrated;
  }, [daemon.activeProfile?.id, daemon.client, queryClient]);

  const persistOrdered = useCallback((session: AgentSession): Promise<AgentSession> => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId) return Promise.reject(new Error('Waku daemon is disconnected'));
    const generation = cacheGenerations.current.get(session.id);
    const previous = persistTails.current.get(session.id);
    const operation = (previous ?? Promise.resolve(session))
      .catch(() => session)
      .then(() => persistSession(client, session))
      .then((saved) => {
        // The reply describes the snapshot that was saved. Runtime events
        // committed while it was in flight are newer than that snapshot, so
        // once the cache has moved on keep it and take only what the daemon
        // owns — turn checkpoints — from the reply.
        if (cacheGenerations.current.get(session.id) === generation) {
          writeSessionCache(saved);
        } else {
          const latest = queryClient.getQueryData<AgentSession>(
            daemonKeys.session(profileId, session.id),
          );
          if (latest) writeSessionCache(withDaemonCheckpoints(latest, saved));
        }
        return saved;
      });
    persistTails.current.set(session.id, operation);
    void operation.finally(() => {
      if (persistTails.current.get(session.id) === operation) persistTails.current.delete(session.id);
    }).catch(() => {});
    return operation;
  }, [daemon.activeProfile?.id, daemon.client, queryClient, writeSessionCache]);

  const schedulePersist = useCallback((sessionId: string) => {
    const profileId = daemon.activeProfile?.id;
    if (!profileId) return;
    const previous = persistTimers.current.get(sessionId);
    if (previous) clearTimeout(previous);
    const timer = setTimeout(() => {
      persistTimers.current.delete(sessionId);
      const latest = queryClient.getQueryData<AgentSession>(
        daemonKeys.session(profileId, sessionId),
      );
      if (latest) void persistOrdered(latest).catch((cause) => {
        setErrors((current) => ({ ...current, [sessionId]: errorMessage(cause) }));
      });
    }, 500);
    persistTimers.current.set(sessionId, timer);
  }, [daemon.activeProfile?.id, persistOrdered, queryClient]);

  const removeRuntime = useCallback((sessionId: string) => {
    const entry = entries.current.get(sessionId);
    entry?.unsubscribe();
    entries.current.delete(sessionId);
    pendingSteers.current.delete(sessionId);
    setRuntimes((current) => removeKey(current, sessionId));
  }, []);

  /** After a settled turn is persisted, submit the oldest queued message as
   * the next turn. Guarded per session so one settle drains one message. */
  const drainQueue = useCallback(async (sessionId: string) => {
    const profileId = daemon.activeProfile?.id;
    if (!profileId || drainingQueues.current.has(sessionId)) return;
    const latest = queryClient.getQueryData<AgentSession>(
      daemonKeys.session(profileId, sessionId),
    );
    if (!latest || latest.status !== 'idle') return;
    const next = latest.queued_messages?.[0];
    if (!next) return;
    drainingQueues.current.add(sessionId);
    try {
      const dequeued = {
        ...latest,
        queued_messages: latest.queued_messages?.slice(1),
      };
      cacheSession(dequeued);
      const persisted = await persistOrdered(dequeued);
      await sendPromptRef.current?.(
        persisted,
        next.display_content ?? next.content,
        next.attachments ?? [],
        next.content,
      );
    } catch (cause) {
      setErrors((values) => ({ ...values, [sessionId]: errorMessage(cause) }));
    } finally {
      drainingQueues.current.delete(sessionId);
    }
  }, [cacheSession, daemon.activeProfile?.id, persistOrdered, queryClient]);

  const subscribe = useCallback((
    session: AgentSession,
    runtimeId: string,
    supportsSteer = false,
    starting = true,
  ): RuntimeEntry => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId) throw new Error('Waku daemon is disconnected');
    const existing = entries.current.get(session.id);
    if (existing) return existing;

    const entry: RuntimeEntry = {
      session,
      runtimeId,
      supportsSteer,
      starting,
      running: sessionIsRunning(session),
      lastDriverError: null,
      unsubscribe: () => {},
      pending: [],
      flushTimer: null,
      lastFlushAt: 0,
    };
    entries.current.set(session.id, entry);
    cacheSession(session);
    setRuntimes((current) => ({ ...current, [session.id]: publicRuntime(entry) }));

    /** Apply one (possibly coalesced) event to `current` and collect its
     * side effects; the caller commits once per flush. */
    interface FlushState {
      current: AgentSession;
      mutated: boolean;
      settled: boolean;
      removeRuntime: boolean;
    }
    const reduceOne = (state: FlushState, event: SequencedEvent) => {
      if (event.event.kind === 'connected' || event.event.kind === 'turnStarted' || event.event.kind === 'turnFinished') {
        entry.lastDriverError = null;
      } else if (event.event.kind === 'error' && typeof event.event.payload === 'string') {
        entry.lastDriverError = event.event.payload;
      }
      if (event.event.kind === 'steerAccepted') {
        const payload = event.event.payload as { message?: string };
        // The provider folded a steer into the live turn. Ours is pending
        // here; another client's is not, and its message belongs in this
        // transcript just the same — the desktop mirrors it too.
        const pending = pendingSteers.current.get(session.id)?.shift();
        const content = payload.message ?? pending?.providerPrompt;
        if (content) {
          state.current = {
            ...state.current,
            messages: [
              ...state.current.messages,
              {
                id: clock.randomUUID(),
                turn_id: state.current.turns.at(-1)?.id ?? null,
                role: 'user',
                content,
                display_content: pending && (
                  pending.attachments.length || pending.providerPrompt !== pending.displayContent
                ) ? pending.displayContent : null,
                attachments: pending?.attachments ?? [],
                created_at: clock.nowSeconds(),
                streaming: false,
              },
            ],
          };
        }
      } else if (event.event.kind === 'steerRejected') {
        const pending = pendingSteers.current.get(session.id)?.shift();
        if (pending) {
          state.current = queueSubmission(
            state.current,
            pending.displayContent,
            clock,
            pending.attachments,
            pending.providerPrompt,
          );
          void persistOrdered(state.current).catch(() => {});
          setErrors((values) => ({
            ...values,
            [session.id]: 'The agent couldn’t take that mid-turn, so it was queued for the next turn.',
          }));
        }
      }
      const result = reduceRuntimeEvent(state.current, event, clock, entry.lastDriverError);
      state.current = result.session;
      state.mutated = true;
      if (result.permission !== undefined) {
        setPermissions((values) => ({ ...values, [session.id]: result.permission ?? undefined }));
      }
      if (result.userInput !== undefined) {
        setUserInputs((values) => ({ ...values, [session.id]: result.userInput ?? undefined }));
      }
      if (result.error) setErrors((values) => ({ ...values, [session.id]: result.error }));
      if (result.settled) state.settled = true;
      if (result.removeRuntime) state.removeRuntime = true;
    };

    /** Drain the buffer as one commit. Adjacent same-kind stream deltas
     * coalesce into a single reduce (the desktop's `pop_stream_batch`), with
     * per-event dedupe gates so a replay straddling the cursor stays exact. */
    const flush = () => {
      entry.flushTimer = null;
      entry.lastFlushAt = Date.now();
      if (entries.current.get(session.id) !== entry) {
        entry.pending.length = 0;
        return;
      }
      const batch = entry.pending.splice(0);
      if (!batch.length) return;
      const key = daemonKeys.session(profileId, session.id);
      const state: FlushState = {
        current: queryClient.getQueryData<AgentSession>(key) ?? entry.session,
        mutated: false,
        settled: false,
        removeRuntime: false,
      };
      let run: { kind: 'textDelta' | 'reasoningDelta'; text: string; envelope: SequencedEvent } | null = null;
      const flushRun = () => {
        if (!run) return;
        reduceOne(state, { ...run.envelope, event: { kind: run.kind, payload: run.text } });
        run = null;
      };
      for (const event of batch) {
        if (!shouldApplyRuntimeEvent(state.current, event)) continue;
        const kind = event.event.kind;
        if ((kind === 'textDelta' || kind === 'reasoningDelta') && typeof event.event.payload === 'string') {
          if (run && run.kind === kind) {
            run.text += event.event.payload;
            run.envelope = event;
          } else {
            flushRun();
            run = { kind, text: event.event.payload, envelope: event };
          }
          continue;
        }
        flushRun();
        reduceOne(state, event);
      }
      flushRun();
      if (!state.mutated) return;
      cacheSession(state.current);
      if (state.settled) {
        const timer = persistTimers.current.get(session.id);
        if (timer) clearTimeout(timer);
        persistTimers.current.delete(session.id);
        void persistOrdered(state.current)
          .then(() => queryClient.invalidateQueries({ queryKey: daemonKeys.taskState(profileId) }))
          .then(() => drainQueue(session.id))
          .catch((cause) => {
            setErrors((values) => ({ ...values, [session.id]: errorMessage(cause) }));
          });
      } else {
        schedulePersist(session.id);
      }
      if (state.removeRuntime) removeRuntime(session.id);
    };

    const unsubscribe = client.subscribe(session.id, runtimeId, (event) => {
      if (entries.current.get(session.id) !== entry) return;
      entry.pending.push(event);
      if (!deferrableEvent(event)) {
        if (entry.flushTimer) {
          clearTimeout(entry.flushTimer);
          entry.flushTimer = null;
        }
        flush();
        return;
      }
      if (entry.flushTimer) return;
      const wait = Math.max(0, STREAM_COMMIT_MS - (Date.now() - entry.lastFlushAt));
      entry.flushTimer = setTimeout(flush, wait);
    });
    const teardown = () => {
      if (entry.flushTimer) {
        clearTimeout(entry.flushTimer);
        entry.flushTimer = null;
      }
      entry.pending.length = 0;
      unsubscribe();
    };
    if (entries.current.get(session.id) === entry) entry.unsubscribe = teardown;
    else teardown();
    return entry;
  }, [cacheSession, daemon.activeProfile?.id, daemon.client, drainQueue, persistOrdered, queryClient, removeRuntime, schedulePersist]);

  const attachSession = useCallback((session: AgentSession): Promise<boolean> => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId || daemon.phase !== 'connected') return Promise.resolve(false);
    if (entries.current.has(session.id)) return Promise.resolve(true);
    const pending = attachRequests.current.get(session.id);
    if (pending) return pending;

    const request = (async () => {
      // The screen may still be displaying a list placeholder. Its empty
      // arrays are not a transcript, so hydrate before replaying any events.
      const hydrated = await loadFullSession(session.id);
      const attached = await attachDaemonSession(client, session.id);
      if (!client.connected || !attached) return false;
      const current = queryClient.getQueryData<AgentSession>(
        daemonKeys.session(profileId, session.id),
      ) ?? hydrated;
      subscribe(current, attached.runtimeId, attached.supportsSteer, false);
      return true;
    })().finally(() => {
      if (attachRequests.current.get(session.id) === request) attachRequests.current.delete(session.id);
    });
    attachRequests.current.set(session.id, request);
    return request;
  }, [daemon.activeProfile?.id, daemon.client, daemon.phase, loadFullSession, queryClient, subscribe]);

  const sendPrompt = useCallback(async (
    inputSession: AgentSession,
    rawPrompt: string,
    attachments: MessageAttachment[] = [],
    providerPromptOverride?: string,
  ): Promise<AgentSession> => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId || daemon.phase !== 'connected') {
      throw new Error('Waku daemon is disconnected');
    }
    const prompt = rawPrompt.trim();
    if (!prompt && attachments.length === 0) return inputSession;
    const providerPrompt = providerPromptOverride === undefined
      ? providerPromptForSubmission(prompt, attachments)
      : providerPromptOverride.trim();
    // The screen can hand over a task-list skeleton while hydration is
    // still in flight; building the turn on that would persist a transcript
    // with only the new messages. Always start from the full session.
    let current = await loadFullSession(inputSession.id);
    if (sessionBusy(current)) {
      const queued = queueSubmission(current, prompt, clock, attachments, providerPrompt);
      cacheSession(queued);
      return persistOrdered(queued);
    }

    if (!entries.current.has(current.id)) await attachSession(current);
    let runtime = entries.current.get(current.id);
    let startup: { binary: string; cwd: string } | null = null;
    if (!runtime) {
      const state = await loadTaskState(client);
      const project = state.projects.find((item) => item.id === current.project_id);
      if (!project) throw new Error('This task’s project is no longer available on the daemon');
      if (current.workspace?.kind === 'newWorktree') {
        current = await materializeWorktree(
          client,
          current,
          project.path,
          prompt || attachments[0]?.name || 'task',
        );
        cacheSession(current);
        current = await persistOrdered(current);
      }
      const settings = await loadDaemonSettings(client);
      const probe = await probeProvider(client, current.provider, settings);
      if (!probe.installed || !probe.path) {
        throw new Error(`${current.provider} is not installed on the daemon host`);
      }
      if (daemon.activeProfile) {
        writeProviderProbeCache(
          persistentStorageSync(),
          daemon.activeProfile.address,
          current.provider,
          settings.provider_binary_overrides?.[current.provider] ?? null,
          probe,
        );
      }
      startup = { binary: probe.path, cwd: sessionCwd(current, project) };
    }

    current = beginTurn(current, prompt, clock, attachments, providerPrompt);
    // The ids beginTurn gave the turn and its user message ride along with
    // the prompt, so every other client attached to the runtime mirrors the
    // same rows instead of minting its own.
    const submitted = submittedTurnIdentity(current);
    cacheSession(current);
    current = await persistOrdered(current);
    try {
      if (!runtime) {
        const runtimeId = Crypto.randomUUID();
        runtime = subscribe(current, runtimeId);
        const response = await client.request({
          type: 'start',
          options: {
            provider: current.provider,
            binary: startup!.binary,
            cwd: startup!.cwd,
            mode: current.runtime_mode,
            model: current.model ?? null,
            reasoningEffort: current.reasoning_effort ?? null,
            serviceTier: current.service_tier ?? null,
            contextWindow: current.context_window ?? null,
            agentPreset: current.agent_preset ?? null,
            computerUseEnabled: false,
            providerCursor: current.provider_cursor as never,
          },
        }, current.id, runtimeId);
        if (response.type !== 'started') {
          throw new Error(`Expected daemon response started, received ${response.type}`);
        }
        runtime.supportsSteer = response.supportsSteer;
        runtime.starting = false;
        setRuntimes((values) => ({ ...values, [current.id]: publicRuntime(runtime!) }));
      }
      await client.request(
        {
          type: 'prompt',
          prompt: providerPrompt,
          turnId: submitted.turnId,
          messageId: submitted.messageId,
        },
        current.id,
        runtime.runtimeId,
      );
      setErrors((values) => removeKey(values, current.id));
      return current;
    } catch (cause) {
      if (runtime?.starting) removeRuntime(current.id);
      const latest = queryClient.getQueryData<AgentSession>(
        daemonKeys.session(profileId, current.id),
      ) ?? current;
      const cursor = latest.runtime_event_cursor;
      const failed = reduceRuntimeEvent(latest, {
        sessionId: latest.id,
        runtimeId: runtime?.runtimeId ?? Crypto.randomUUID(),
        epoch: cursor?.epoch ?? `mobile-${Date.now()}`,
        sequence: (cursor?.sequence ?? 0) + 1,
        event: { kind: 'turnFinished', payload: { success: false, summary: errorMessage(cause) } },
      }, clock).session;
      cacheSession(failed);
      await persistOrdered(failed).catch(() => failed);
      throw cause;
    }
  }, [attachSession, cacheSession, daemon.activeProfile?.id, daemon.client, daemon.phase, loadFullSession, persistOrdered, queryClient, removeRuntime, subscribe]);

  useEffect(() => {
    sendPromptRef.current = sendPrompt;
  }, [sendPrompt]);

  /** Inject a prompt into the running turn when the provider supports it;
   * otherwise fall through to sendPrompt, which queues while busy. */
  const steerPrompt = useCallback(async (
    session: AgentSession,
    rawPrompt: string,
    attachments: MessageAttachment[] = [],
    providerPromptOverride?: string,
  ) => {
    const client = daemon.client;
    const prompt = rawPrompt.trim();
    if (!prompt && attachments.length === 0) return;
    const providerPrompt = providerPromptOverride === undefined
      ? providerPromptForSubmission(prompt, attachments)
      : providerPromptOverride.trim();
    if (!client || daemon.phase !== 'connected') throw new Error('Waku daemon is disconnected');
    const runtime = entries.current.get(session.id);
    if (
      !runtime || !runtime.supportsSteer ||
      session.status === 'connecting' || session.status === 'idle' || session.status === 'failed'
    ) {
      await sendPrompt(session, prompt, attachments, providerPrompt);
      return;
    }
    const pending = pendingSteers.current.get(session.id) ?? [];
    pending.push({ providerPrompt, displayContent: prompt, attachments });
    pendingSteers.current.set(session.id, pending);
    await client.request({ type: 'steer', prompt: providerPrompt }, session.id, runtime.runtimeId);
  }, [daemon.client, daemon.phase, sendPrompt]);

  const createTask = useCallback(async (
    projectId: string,
    provider: ProviderKind,
    isolated: boolean,
    prompt: string,
    options: NewSessionOptions = {},
    providerPromptOverride?: string,
  ): Promise<AgentSession> => {
    const profileId = daemon.activeProfile?.id;
    if (!profileId || !daemon.client || daemon.phase !== 'connected') {
      throw new Error('Waku daemon is disconnected');
    }
    const draft = createSession(projectId, provider, isolated, clock, options);
    cacheSession(draft);
    const saved = await persistOrdered(draft);
    try {
      return await sendPrompt(saved, prompt, [], providerPromptOverride);
    } catch (cause) {
      setErrors((values) => ({ ...values, [saved.id]: errorMessage(cause) }));
      return queryClient.getQueryData<AgentSession>(
        daemonKeys.session(profileId, saved.id),
      ) ?? saved;
    }
  }, [cacheSession, daemon.activeProfile?.id, daemon.client, daemon.phase, persistOrdered, queryClient, sendPrompt]);

  /** Local /goal commands use the native provider operation without adding
   * a user prompt. A fresh task still needs a subscribed provider runtime;
   * its goalUpdated/turnStarted events supply the authoritative state. */
  const sendGoalOperation = useCallback(async (inputSession: AgentSession, operation: GoalOperation) => {
    const client = daemon.client;
    if (!client || daemon.phase !== 'connected') throw new Error('Waku daemon is disconnected');
    let current = await loadFullSession(inputSession.id);
    if (current.provider !== 'codex') throw new Error('Goals require Codex');
    if (!entries.current.has(current.id)) await attachSession(current);
    let runtime = entries.current.get(current.id);
    if (!runtime) {
      const state = await loadTaskState(client);
      const project = state.projects.find((item) => item.id === current.project_id);
      if (!project) throw new Error('This task’s project is no longer available on the daemon');
      const settings = await loadDaemonSettings(client);
      const probe = await probeProvider(client, current.provider, settings);
      if (!probe.installed || !probe.path) throw new Error('Codex is not installed on the daemon host');
      current = await materializeWorktree(
        client, current, project.path,
        operation.kind === 'set' ? operation.objective || 'goal' : 'goal',
      );
      cacheSession(current);
      current = await persistOrdered(current);
      const runtimeId = Crypto.randomUUID();
      runtime = subscribe(current, runtimeId);
      try {
        const response = await client.request({
          type: 'start',
          options: {
            provider: current.provider,
            binary: probe.path,
            cwd: sessionCwd(current, project),
            mode: current.runtime_mode,
            model: current.model ?? null,
            reasoningEffort: current.reasoning_effort ?? null,
            serviceTier: current.service_tier ?? null,
            contextWindow: current.context_window ?? null,
            agentPreset: current.agent_preset ?? null,
            computerUseEnabled: false,
            providerCursor: current.provider_cursor as never,
          },
        }, current.id, runtimeId);
        if (response.type !== 'started') throw new Error('The daemon could not start Codex');
        runtime.supportsSteer = response.supportsSteer;
        runtime.starting = false;
        setRuntimes((values) => ({ ...values, [current.id]: publicRuntime(runtime!) }));
      } catch (cause) {
        removeRuntime(current.id);
        throw cause;
      }
    }
    await client.request({ type: 'goal', operation }, current.id, runtime.runtimeId);
  }, [attachSession, cacheSession, daemon.client, daemon.phase, loadFullSession, persistOrdered, removeRuntime, subscribe]);

  const cancel = useCallback(async (sessionId: string) => {
    const client = daemon.client;
    if (!client) throw new Error('Waku daemon is disconnected');
    const runtime = entries.current.get(sessionId);
    if (!runtime) throw new Error('This task has no live agent runtime');
    await client.request({ type: 'cancel' }, sessionId, runtime.runtimeId);
  }, [daemon.client]);

  const markWorking = useCallback((sessionId: string) => {
    const profileId = daemon.activeProfile?.id;
    if (!profileId) return;
    const key = daemonKeys.session(profileId, sessionId);
    const session = queryClient.getQueryData<AgentSession>(key);
    if (!session) return;
    const updated = { ...session, status: 'working' as const };
    cacheSession(updated);
    void persistOrdered(updated).catch((cause) => {
      setErrors((values) => ({ ...values, [sessionId]: errorMessage(cause) }));
    });
  }, [cacheSession, daemon.activeProfile?.id, persistOrdered, queryClient]);

  const respond = useCallback(async (sessionId: string, requestId: string, optionId: string) => {
    const client = daemon.client;
    const runtime = entries.current.get(sessionId);
    if (!client || !runtime) throw new Error('This task has no live agent runtime');
    await client.request({ type: 'respond', requestId, optionId }, sessionId, runtime.runtimeId);
    setPermissions((values) => ({ ...values, [sessionId]: undefined }));
    markWorking(sessionId);
  }, [daemon.client, markWorking]);

  const respondUserInput = useCallback(async (
    sessionId: string,
    requestId: string,
    answers: UserInputAnswer[],
  ) => {
    const client = daemon.client;
    const runtime = entries.current.get(sessionId);
    if (!client || !runtime) throw new Error('This task has no live agent runtime');
    await client.request(
      { type: 'respondUserInput', requestId, answers },
      sessionId,
      runtime.runtimeId,
    );
    setUserInputs((values) => ({ ...values, [sessionId]: undefined }));
    markWorking(sessionId);
  }, [daemon.client, markWorking]);

  /** Change model traits / access mode. Applied live via
   * applyOptions when a runtime exists; a runtime that can't take the change
   * is closed so the next prompt restarts with the new options. */
  const updateSessionOptions = useCallback(async (
    sessionId: string,
    changes: SessionOptionChanges,
  ) => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId || daemon.phase !== 'connected') {
      throw new Error('Waku daemon is disconnected');
    }
    const current = await loadFullSession(sessionId);
    const next = applySessionOptions(current, changes, clock);
    cacheSession(next);
    try {
      const runtime = entries.current.get(sessionId);
      if (runtime && !runtime.starting) {
        const response = await client.request({
          type: 'applyOptions',
          options: {
            mode: next.runtime_mode,
            model: next.model ?? null,
            reasoningEffort: next.reasoning_effort ?? null,
            serviceTier: next.service_tier ?? null,
            contextWindow: next.context_window ?? null,
          },
        }, sessionId, runtime.runtimeId);
        if (response.type !== 'optionsApplied' || !response.applied) {
          await client.request({ type: 'closeSession' }, sessionId, runtime.runtimeId);
          removeRuntime(sessionId);
        }
      }
      await persistOrdered(next);
    } catch (cause) {
      cacheSession(current);
      throw cause;
    }
  }, [cacheSession, daemon.activeProfile?.id, daemon.client, daemon.phase, loadFullSession, persistOrdered, removeRuntime]);

  const renameSession = useCallback(async (sessionId: string, title: string) => {
    const current = await loadFullSession(sessionId);
    const trimmed = title.trim();
    const next = {
      ...current,
      title: trimmed || 'New task',
      updated_at: clock.nowSeconds(),
    };
    cacheSession(next);
    await persistOrdered(next);
  }, [cacheSession, loadFullSession, persistOrdered]);

  const deleteSession = useCallback(async (sessionId: string) => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId || daemon.phase !== 'connected') {
      throw new Error('Waku daemon is disconnected');
    }
    const runtime = entries.current.get(sessionId);
    if (runtime) {
      await client.request({ type: 'closeSession' }, sessionId, runtime.runtimeId).catch(() => {});
      removeRuntime(sessionId);
    }
    const timer = persistTimers.current.get(sessionId);
    if (timer) clearTimeout(timer);
    persistTimers.current.delete(sessionId);
    await persistTails.current.get(sessionId)?.catch(() => {});
    await removeDaemonSession(client, sessionId);
    queryClient.removeQueries({ queryKey: daemonKeys.session(profileId, sessionId) });
    queryClient.setQueryData<TaskState>(daemonKeys.taskState(profileId), (current) => (
      current
        ? { ...current, sessions: current.sessions.filter((item) => item.id !== sessionId) }
        : current
    ));
    setPermissions((values) => removeKey(values, sessionId));
    setUserInputs((values) => removeKey(values, sessionId));
    setErrors((values) => removeKey(values, sessionId));
  }, [daemon.activeProfile?.id, daemon.client, daemon.phase, queryClient, removeRuntime]);

  const removeQueuedMessage = useCallback(async (sessionId: string, messageId: string) => {
    const profileId = daemon.activeProfile?.id;
    if (!profileId) throw new Error('Waku daemon is disconnected');
    const current = queryClient.getQueryData<AgentSession>(
      daemonKeys.session(profileId, sessionId),
    );
    if (!current) return;
    const next = {
      ...current,
      queued_messages: (current.queued_messages ?? []).filter((item) => item.id !== messageId),
    };
    cacheSession(next);
    await persistOrdered(next);
  }, [cacheSession, daemon.activeProfile?.id, persistOrdered, queryClient]);

  const dismissError = useCallback((sessionId: string) => {
    setErrors((values) => removeKey(values, sessionId));
  }, []);

  // A request in flight when the socket drops rejects with a transport error.
  // The connection banner owns that outage; once the link is live again the
  // task must not keep displaying the old rejection indefinitely.
  useEffect(() => {
    if (!hasRecoveredDisconnectError) return;
    setErrors(clearDaemonDisconnectErrors);
  }, [hasRecoveredDisconnectError]);

  // After a reconnect the daemon replays whatever each followed runtime
  // emitted while the link was down, so streams resume by themselves. What
  // it cannot replay is a runtime that no longer exists (the daemon
  // restarted) or one another client replaced: confirm each entry's runtime
  // and either drop it, so the next prompt starts fresh, or follow the new
  // one, whose events are already buffered on the client.
  useEffect(() => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId || daemon.phase !== 'connected') return;
    if (daemon.connections <= 1 || revalidatedConnections.current === daemon.connections) return;
    revalidatedConnections.current = daemon.connections;
    for (const [sessionId, entry] of entries.current) {
      void attachDaemonSession(client, sessionId).then((attached) => {
        if (entries.current.get(sessionId) !== entry) return;
        if (attached?.runtimeId === entry.runtimeId) return;
        removeRuntime(sessionId);
        if (attached) {
          const current = queryClient.getQueryData<AgentSession>(
            daemonKeys.session(profileId, sessionId),
          );
          if (current) subscribe(current, attached.runtimeId, attached.supportsSteer, false);
        }
        void queryClient.invalidateQueries({
          queryKey: daemonKeys.session(profileId, sessionId),
        });
      }).catch(() => {});
    }
  }, [
    daemon.activeProfile?.id,
    daemon.client,
    daemon.connections,
    daemon.phase,
    queryClient,
    removeRuntime,
    subscribe,
  ]);

  // Another client (the desktop, the web app) persisting a session announces
  // itself through taskStateChanged. Refetch any hydrated session we are not
  // already following live, so watching a desktop-driven task stays current.
  useEffect(() => {
    const client = daemon.client;
    const profileId = daemon.activeProfile?.id;
    if (!client || !profileId) return;
    return client.subscribeTaskState(() => {
      void queryClient.invalidateQueries({
        queryKey: ['daemon', profileId, 'session'],
        predicate: (query) => {
          const sessionId = query.queryKey[3];
          return typeof sessionId === 'string' && !entries.current.has(sessionId);
        },
      });
    });
  }, [daemon.activeProfile?.id, daemon.client, queryClient]);

  useEffect(() => {
    setRuntimes({});
    setPermissions({});
    setUserInputs({});
    setErrors({});
    revalidatedConnections.current = 0;
    return () => {
      for (const entry of entries.current.values()) entry.unsubscribe();
      entries.current.clear();
      attachRequests.current.clear();
      persistTails.current.clear();
      pendingSteers.current.clear();
      drainingQueues.current.clear();
      for (const timer of persistTimers.current.values()) clearTimeout(timer);
      persistTimers.current.clear();
    };
  }, [daemon.activeProfile?.id, daemon.client]);

  return (
    <RuntimeContext.Provider value={{
      runtimes,
      permissions,
      userInputs,
      errors,
      attachSession,
      sendPrompt,
      steerPrompt,
      createTask,
      cancel,
      respond,
      respondUserInput,
      updateSessionOptions,
      sendGoalOperation,
      renameSession,
      deleteSession,
      removeQueuedMessage,
      dismissError,
    }}>
      {children}
    </RuntimeContext.Provider>
  );
}

export function useRuntime() {
  const context = useContext(RuntimeContext);
  if (!context) throw new Error('useRuntime must be used inside RuntimeProvider');
  return context;
}

function publicRuntime(entry: RuntimeEntry): MobileRuntime {
  return {
    runtimeId: entry.runtimeId,
    supportsSteer: entry.supportsSteer,
    starting: entry.starting,
    running: entry.running,
  };
}

function removeKey<T>(record: Record<string, T | undefined>, key: string): Record<string, T | undefined> {
  const next = { ...record };
  delete next[key];
  return next;
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error && cause.message.trim() ? cause.message : String(cause);
}

/** The daemon captures ending checkpoints itself, so a reply's checkpoint for
 * a turn is the one part of it that can be newer than the local cache. */
function withDaemonCheckpoints(latest: AgentSession, saved: AgentSession): AgentSession {
  let changed = false;
  const turns = latest.turns.map((turn) => {
    const stored = saved.turns.find((candidate) => candidate.turn_count === turn.turn_count);
    if (
      !stored?.checkpoint
      || JSON.stringify(stored.checkpoint) === JSON.stringify(turn.checkpoint)
    ) {
      return turn;
    }
    changed = true;
    return { ...turn, checkpoint: stored.checkpoint };
  });
  return changed ? { ...latest, turns } : latest;
}
