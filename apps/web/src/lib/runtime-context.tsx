import { useQueryClient } from '@tanstack/react-query'
import type {
  AgentSession,
  DaemonSettings,
  MessageAttachment,
  PlanUsage,
  Project,
  ProviderProbe,
  SequencedEvent,
  UserInputAnswer,
} from '@waku/client'
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { toast } from 'sonner'
import { useDaemon } from './daemon-context'
import { translate, useI18n } from './i18n'
import {
  attachSession as attachDaemonSession,
  beginTurn,
  captureTurnCheckpoint,
  captureTurnStart,
  daemonKeys,
  loadDaemonSettings,
  loadTaskState,
  materializeWorktree,
  persistSession,
  probeProvider,
  sessionCwd,
  type TaskState,
} from './daemon-api'
import {
  browserProviderProbeStorage,
  PROVIDER_PROBE_CACHE_STALE_TIME,
  writeProviderProbeCache,
} from './provider-probe-cache'
import {
  reduceRuntimeEvent,
  type PendingPermission,
  type PendingUserInput,
} from './event-reducer'

interface RuntimeSummary {
  runtimeId: string
  supportsSteer: boolean
  starting: boolean
}

interface RuntimeEntry extends RuntimeSummary {
  lastDriverError: string | null
  unsubscribe: () => void
}

export type BackgroundWorkKind = 'process' | 'monitor' | 'subagent'
export type BackgroundWorkStatus =
  | 'starting'
  | 'running'
  | 'monitoring'
  | 'stopping'
  | 'completed'
  | 'failed'
  | 'stopped'
  | 'lost'

export interface BackgroundWorkKey {
  kind: BackgroundWorkKind
  providerId: string
}

export interface BackgroundWorkItem {
  key: BackgroundWorkKey
  title: string
  detail: string | null
  command: string | null
  cwd: string | null
  output: string | null
  outputTruncated: boolean
  startedAtMs: number
  updatedAtMs: number
  durationMs: number | null
  exitCode: number | null
  background: boolean
  canStop: boolean
  controlId: string | null
  originActivityId: string | null
  role: string | null
  model: string | null
  parentId: string | null
  status: BackgroundWorkStatus
}

type BackgroundWorkEvent =
  | { type: 'upsert'; item: BackgroundWorkItem }
  | { type: 'outputDelta'; key: BackgroundWorkKey; delta: string }
  | { type: 'reconcileProcesses'; items: BackgroundWorkItem[] }
  | { type: 'reconcileLive'; items: BackgroundWorkItem[] }
  | { type: 'stopRequested'; key: BackgroundWorkKey }
  | { type: 'stopFailed'; key: BackgroundWorkKey; message: string }

interface RuntimeContextValue {
  runtimes: Record<string, RuntimeSummary>
  permissions: Record<string, PendingPermission | undefined>
  userInputs: Record<string, PendingUserInput | undefined>
  backgroundWork: Record<string, BackgroundWorkItem[]>
  responseForks: Record<string, number | undefined>
  messageRewinds: Record<string, number | undefined>
  attachSession: (session: AgentSession) => Promise<boolean>
  sendPrompt: (
    session: AgentSession,
    prompt: string,
    attachments?: MessageAttachment[],
    providerPromptOverride?: string,
  ) => Promise<void>
  steerPrompt: (
    session: AgentSession,
    prompt: string,
    attachments?: MessageAttachment[],
    providerPromptOverride?: string,
  ) => Promise<void>
  cancel: (sessionId: string) => Promise<void>
  closeSession: (sessionId: string) => Promise<void>
  removeQueuedMessage: (sessionId: string, messageId: string) => Promise<void>
  respond: (sessionId: string, requestId: string, optionId: string) => Promise<void>
  respondUserInput: (
    sessionId: string,
    requestId: string,
    answers: UserInputAnswer[],
  ) => Promise<void>
  refreshBackgroundWork: (sessionId: string) => Promise<void>
  stopBackgroundWork: (sessionId: string, item: BackgroundWorkItem) => Promise<void>
  forkSessionFromResponse: (
    session: AgentSession,
    turnCount: number,
  ) => Promise<{ session: AgentSession; checkpointWarning: string | null }>
  rewindSessionToMessage: (
    session: AgentSession,
    turnCount: number,
    prompt: string,
    attachments?: MessageAttachment[],
  ) => Promise<{ session: AgentSession; cleanupWarning: string | null }>
  saveSession: (session: AgentSession, project?: Project) => Promise<AgentSession>
}

const RuntimeContext = createContext<RuntimeContextValue | null>(null)

export function RuntimeProvider({ children }: { children: ReactNode }) {
  const { client, config, phase } = useDaemon()
  const { locale } = useI18n()
  const localeRef = useRef(locale)
  localeRef.current = locale
  const queryClient = useQueryClient()
  const entries = useRef(new Map<string, RuntimeEntry>())
  const attachRequests = useRef(new Map<string, Promise<boolean>>())
  const persistenceTails = useRef(new Map<string, Promise<AgentSession>>())
  const checkpointCaptures = useRef(new Map<string, Promise<AgentSession>>())
  const projectionPersistTimers = useRef(new Map<string, number>())
  const saveGenerations = useRef(new Map<string, number>())
  const sendPromptRef = useRef<RuntimeContextValue['sendPrompt'] | null>(null)
  const pendingSteers = useRef(
    new Map<
      string,
      Array<{
        providerPrompt: string
        displayContent: string
        attachments: MessageAttachment[]
      }>
    >(),
  )
  const responseForksInFlight = useRef(new Map<string, number>())
  const messageRewindsInFlight = useRef(new Map<string, number>())
  const [runtimes, setRuntimes] = useState<Record<string, RuntimeSummary>>({})
  const [permissions, setPermissions] = useState<
    Record<string, PendingPermission | undefined>
  >({})
  const [userInputs, setUserInputs] = useState<
    Record<string, PendingUserInput | undefined>
  >({})
  const [backgroundWork, setBackgroundWork] = useState<Record<string, BackgroundWorkItem[]>>({})
  const [responseForks, setResponseForks] = useState<Record<string, number | undefined>>({})
  const [messageRewinds, setMessageRewinds] = useState<Record<string, number | undefined>>({})

  const cacheSession = useCallback(
    (session: AgentSession, project?: Project) => {
      if (!config) return
      queryClient.setQueryData(
        daemonKeys.session(config.address, session.id),
        session,
      )
      queryClient.setQueryData<TaskState>(
        daemonKeys.taskState(config.address),
        (current) => {
          if (!current) return current
          const projects = project
            ? current.projects.some((item) => item.id === project.id)
              ? current.projects.map((item) => (item.id === project.id ? project : item))
              : [...current.projects, project]
            : current.projects
          const sessions = current.sessions.some((item) => item.id === session.id)
            ? current.sessions.map((item) =>
                item.id === session.id ? mergeSessionSummary(item, session) : item,
              )
            : [...current.sessions, session]
          return { ...current, projects, sessions }
        },
      )
    },
    [config, queryClient],
  )

  const persistOrdered = useCallback(
    (session: AgentSession, project?: Project) => {
      if (!client) {
        return Promise.reject(new Error(translate(localeRef.current, 'errors.daemon_disconnected')))
      }
      const previous = persistenceTails.current.get(session.id)
      const operation = (previous ?? Promise.resolve(session))
        .catch(() => session)
        .then(() => persistSession(client, session, project))
      persistenceTails.current.set(session.id, operation)
      void operation
        .finally(() => {
          if (persistenceTails.current.get(session.id) === operation) {
            persistenceTails.current.delete(session.id)
          }
        })
        .catch(() => {})
      return operation
    },
    [client],
  )

  const scheduleProjectionPersist = useCallback(
    (sessionId: string) => {
      if (!config) return
      const pending = projectionPersistTimers.current.get(sessionId)
      if (pending !== undefined) window.clearTimeout(pending)
      const timer = window.setTimeout(() => {
        projectionPersistTimers.current.delete(sessionId)
        const latest = queryClient.getQueryData<AgentSession>(
          daemonKeys.session(config.address, sessionId),
        )
        if (latest) {
          void persistOrdered(latest).catch((error) => toast.error(errorMessage(error)))
        }
      }, 500)
      projectionPersistTimers.current.set(sessionId, timer)
    },
    [config, persistOrdered, queryClient],
  )

  const removeRuntime = useCallback((sessionId: string) => {
    const entry = entries.current.get(sessionId)
    entry?.unsubscribe()
    entries.current.delete(sessionId)
    setRuntimes((current) => {
      const next = { ...current }
      delete next[sessionId]
      return next
    })
    setBackgroundWork((current) => current[sessionId]
      ? { ...current, [sessionId]: markBackgroundWorkLost(current[sessionId]) }
      : current)
  }, [])

  const saveSession = useCallback(
    async (session: AgentSession, project?: Project) => {
      if (!client || phase !== 'connected') {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      const previous = config
        ? queryClient.getQueryData<AgentSession>(
            daemonKeys.session(config.address, session.id),
          )
        : undefined
      const generation = (saveGenerations.current.get(session.id) ?? 0) + 1
      saveGenerations.current.set(session.id, generation)
      if (previous) cacheSession(session, project)
      try {
        const runtime = entries.current.get(session.id)
        const requiresRuntimeReset = Boolean(
          runtime
            && previous
            && (
              previous.provider !== session.provider
                || previous.agent_preset !== session.agent_preset
            ),
        )
        if (runtime && requiresRuntimeReset) {
          await client.request({ type: 'closeSession' }, session.id, runtime.runtimeId)
          removeRuntime(session.id)
        } else if (runtime) {
          const response = await client.request(
            {
              type: 'applyOptions',
              options: {
                mode: session.runtime_mode,
                interactionMode: session.interaction_mode,
                model: session.model ?? null,
                reasoningEffort: session.reasoning_effort ?? null,
                serviceTier: session.service_tier ?? null,
                contextWindow: session.context_window ?? null,
              },
            },
            session.id,
            runtime.runtimeId,
          )
          if (response.type !== 'optionsApplied' || !response.applied) {
            await client.request({ type: 'closeSession' }, session.id, runtime.runtimeId)
            removeRuntime(session.id)
          }
        }
        const saved = await persistOrdered(session, project)
        if (saveGenerations.current.get(session.id) === generation) {
          cacheSession(saved, project)
          saveGenerations.current.delete(session.id)
        }
        return saved
      } catch (error) {
        if (saveGenerations.current.get(session.id) === generation) {
          if (previous) cacheSession(previous)
          saveGenerations.current.delete(session.id)
        }
        throw error
      }
    },
    [client, config, phase, queryClient, cacheSession, persistOrdered, removeRuntime],
  )

  const finalizeSettledTurn = useCallback(
    async (settled: AgentSession): Promise<AgentSession> => {
      if (!client || !config) {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }

      let saved = await persistOrdered(settled)
      cacheSession(saved)
      const turn = [...saved.turns]
        .reverse()
        .find((candidate) => candidate.status !== 'running' && !candidate.checkpoint)
      if (!turn) return saved

      const taskState = queryClient.getQueryData<TaskState>(
        daemonKeys.taskState(config.address),
      ) ?? await loadTaskState(client)
      const project = taskState.projects.find((candidate) => candidate.id === saved.project_id)
      if (!project) return saved

      let checkpoint: NonNullable<AgentSession['turns'][number]['checkpoint']>
      try {
        checkpoint = await captureTurnCheckpoint(
          client,
          sessionCwd(saved, project),
          saved.id,
          turn.turn_count,
        )
      } catch (error) {
        toast.error(translate(localeRef.current, 'errors.capture_turn_checkpoint', {
          error: errorMessage(error),
        }))
        checkpoint = {
          turn_count: turn.turn_count,
          git_ref: `refs/waku/session-${saved.id}-turn-${turn.turn_count}`,
          status: 'error',
          files: [],
          additions: 0,
          deletions: 0,
          created_at: Math.floor(Date.now() / 1_000),
        }
      }

      const latest = queryClient.getQueryData<AgentSession>(
        daemonKeys.session(config.address, saved.id),
      ) ?? saved
      if (!latest.turns.some((candidate) => candidate.turn_count === turn.turn_count)) {
        return latest
      }
      saved = {
        ...latest,
        turns: latest.turns.map((candidate) => candidate.turn_count === turn.turn_count
          ? { ...candidate, checkpoint }
          : candidate),
      }
      cacheSession(saved)
      return persistOrdered(saved)
    },
    [client, config, queryClient, cacheSession, persistOrdered],
  )

  const finishSettledTurn = useCallback(
    (settled: AgentSession) => {
      if (checkpointCaptures.current.has(settled.id)) return
      const operation = finalizeSettledTurn(settled)
      checkpointCaptures.current.set(settled.id, operation)
      void operation
        .then(async (saved) => {
          if (checkpointCaptures.current.get(saved.id) !== operation) return
          // Release the checkpoint gate before starting the queued turn. The
          // next send must become a real turn instead of queueing itself again.
          checkpointCaptures.current.delete(saved.id)
          if (!config || saved.status !== 'idle') return
          const latest = queryClient.getQueryData<AgentSession>(
            daemonKeys.session(config.address, saved.id),
          ) ?? saved
          const nextQueued = latest.queued_messages?.[0]
          if (!nextQueued) return
          const dequeued = {
            ...latest,
            queued_messages: latest.queued_messages?.slice(1),
          }
          cacheSession(dequeued)
          const persisted = await persistOrdered(dequeued)
          await sendPromptRef.current?.(
            persisted,
            nextQueued.display_content ?? nextQueued.content,
            nextQueued.attachments ?? [],
            nextQueued.content,
          )
        })
        .catch((error) => toast.error(errorMessage(error)))
        .finally(() => {
          if (checkpointCaptures.current.get(settled.id) === operation) {
            checkpointCaptures.current.delete(settled.id)
          }
        })
    },
    [config, queryClient, cacheSession, persistOrdered, finalizeSettledTurn],
  )

  const subscribe = useCallback(
    (session: AgentSession, runtimeId: string) => {
      if (!client || !config) {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      const entry: RuntimeEntry = {
        runtimeId,
        supportsSteer: false,
        starting: true,
        lastDriverError: null,
        unsubscribe: () => {},
      }
      entries.current.set(session.id, entry)
      setRuntimes((current) => ({ ...current, [session.id]: publicRuntime(entry) }))
      const unsubscribe = client.subscribe(session.id, runtimeId, (event) => {
        const key = daemonKeys.session(config.address, session.id)
        let current = queryClient.getQueryData<AgentSession>(key)
        if (!current || runtimeEventAlreadyApplied(current, event)) return
        if (event.event.kind === 'connected' || event.event.kind === 'turnStarted' || event.event.kind === 'turnFinished') {
          entry.lastDriverError = null
        } else if (event.event.kind === 'error' && typeof event.event.payload === 'string') {
          entry.lastDriverError = event.event.payload
        } else if (event.event.kind === 'planUsageUpdated') {
          queryClient.setQueryData<PlanUsage>(
            daemonKeys.planUsage(config.address, current.provider),
            event.event.payload as PlanUsage,
          )
        }
        if (event.event.kind === 'backgroundWork') {
          const backgroundEvent = decodeBackgroundWorkEvent(event.event.payload)
          if (backgroundEvent) {
            setBackgroundWork((current) => ({
              ...current,
              [session.id]: reduceBackgroundWork(current[session.id] ?? [], backgroundEvent),
            }))
          }
        }
        if (event.event.kind === 'steerAccepted') {
          const payload = event.event.payload as { message?: string }
          const pending = pendingSteers.current.get(session.id)?.shift()
          if (pending) {
            const turnId = current.turns.at(-1)?.id ?? null
            current = {
              ...current,
              messages: [
                ...current.messages,
                {
                  id: crypto.randomUUID(),
                  turn_id: turnId,
                  role: 'user',
                  content: payload.message ?? pending.providerPrompt,
                  display_content:
                    pending.attachments.length || pending.providerPrompt !== pending.displayContent
                      ? pending.displayContent
                      : null,
                  attachments: pending.attachments,
                  created_at: Math.floor(Date.now() / 1_000),
                  streaming: false,
                },
              ],
            }
          }
        } else if (event.event.kind === 'steerRejected') {
          const pending = pendingSteers.current.get(session.id)?.shift()
          if (pending) {
            current = queueSubmission(
              current,
              pending.displayContent,
              pending.providerPrompt,
              pending.attachments,
            )
            cacheSession(current)
            void persistOrdered(current).catch((error) => toast.error(errorMessage(error)))
            toast.error(translate(localeRef.current, 'session.steer_rejected_plain'))
          }
        }
        const result = reduceRuntimeEvent(current, event, undefined, entry.lastDriverError)
        cacheSession(result.session)
        scheduleProjectionPersist(session.id)
        if (result.permission !== undefined) {
          setPermissions((previous) => ({
            ...previous,
            [session.id]: result.permission ?? undefined,
          }))
        }
        if (result.userInput !== undefined) {
          setUserInputs((previous) => ({
            ...previous,
            [session.id]: result.userInput ?? undefined,
          }))
        }
        if (result.error) toast.error(result.error)
        if (result.settled) {
          const projectionTimer = projectionPersistTimers.current.get(session.id)
          if (projectionTimer !== undefined) {
            window.clearTimeout(projectionTimer)
            projectionPersistTimers.current.delete(session.id)
          }
          void Promise.all([
            queryClient.invalidateQueries({
              queryKey: ['daemon', config.address, 'workspace'],
            }),
            queryClient.invalidateQueries({
              queryKey: ['daemon', config.address, 'workspace-tree'],
            }),
            queryClient.invalidateQueries({
              queryKey: ['daemon', config.address, 'workspace-diff'],
            }),
            queryClient.invalidateQueries({
              queryKey: daemonKeys.sessionTurnRefsRoot(config.address),
            }),
            queryClient.invalidateQueries({
              queryKey: daemonKeys.composerSources(config.address),
            }),
          ])
          finishSettledTurn(result.session)
        }
        if (result.removeRuntime) removeRuntime(session.id)
      })
      if (entries.current.get(session.id) === entry) {
        entry.unsubscribe = unsubscribe
      } else {
        unsubscribe()
      }
      return entry
    },
    [
      client,
      config,
      queryClient,
      cacheSession,
      removeRuntime,
      persistOrdered,
      scheduleProjectionPersist,
      finishSettledTurn,
    ],
  )

  const attachSession = useCallback<RuntimeContextValue['attachSession']>(
    (session) => {
      if (!client || !config || phase !== 'connected') return Promise.resolve(false)
      if (entries.current.has(session.id)) return Promise.resolve(true)
      const pending = attachRequests.current.get(session.id)
      if (pending) return pending

      const request = (async () => {
        const attached = await attachDaemonSession(client, session.id)
        if (!client.connected) return false
        if (!attached || entries.current.has(session.id)) {
          return entries.current.has(session.id)
        }
        const current = queryClient.getQueryData<AgentSession>(
          daemonKeys.session(config.address, session.id),
        ) ?? session
        const runtime = subscribe(current, attached.runtimeId)
        if (entries.current.get(session.id) !== runtime) return false
        runtime.supportsSteer = attached.supportsSteer
        runtime.starting = false
        setRuntimes((previous) => ({
          ...previous,
          [session.id]: publicRuntime(runtime),
        }))
        return true
      })().finally(() => {
        if (attachRequests.current.get(session.id) === request) {
          attachRequests.current.delete(session.id)
        }
      })
      attachRequests.current.set(session.id, request)
      return request
    },
    [client, config, phase, queryClient, subscribe],
  )

  const sendPrompt = useCallback(
    async (
      inputSession: AgentSession,
      rawPrompt: string,
      attachments: MessageAttachment[] = [],
      providerPromptOverride?: string,
    ) => {
      if (!client || !config || phase !== 'connected') {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      const prompt = rawPrompt.trim()
      if (!prompt && attachments.length === 0) return
      const providerPrompt = providerPromptOverride === undefined
        ? [
            prompt,
            attachments.map((attachment) => `@${attachment.mention}`).join(' '),
          ]
            .filter(Boolean)
            .join(' ')
        : providerPromptOverride.trim()
      const currentSession = queryClient.getQueryData<AgentSession>(
        daemonKeys.session(config.address, inputSession.id),
      ) ?? inputSession
      if (
        currentSession.status === 'connecting' ||
        currentSession.status === 'working' ||
        currentSession.status === 'waiting' ||
        checkpointCaptures.current.has(currentSession.id)
      ) {
        const queued = queueSubmission(currentSession, prompt, providerPrompt, attachments)
        cacheSession(queued)
        await persistOrdered(queued)
        return
      }

      if (!entries.current.has(currentSession.id)) {
        await attachSession(currentSession)
      }

      let session = beginTurn(currentSession, prompt, attachments)
      cacheSession(session)

      let project: Project
      let runtime = entries.current.get(currentSession.id)
      let startup: { probe: ProviderProbe; settings: DaemonSettings } | null = null
      try {
        const state = await loadTaskState(client)
        const foundProject = state.projects.find((item) => item.id === currentSession.project_id)
        if (!foundProject) throw new Error(translate(localeRef.current, 'errors.task_project_not_found'))
        project = foundProject

        if (!runtime) {
          const settings = await queryClient.fetchQuery({
            queryKey: daemonKeys.settings(config.address),
            queryFn: () => loadDaemonSettings(client),
            staleTime: 60_000,
          })
          const binaryOverride = settings.provider_binary_overrides?.[currentSession.provider] ?? null
          const providerProbe = await queryClient.fetchQuery({
            queryKey: daemonKeys.provider(config.address, currentSession.provider, binaryOverride),
            queryFn: async () => {
              const data = await probeProvider(client, currentSession.provider, settings)
              writeProviderProbeCache(
                browserProviderProbeStorage(),
                config.address,
                currentSession.provider,
                binaryOverride,
                data,
              )
              return data
            },
            staleTime: PROVIDER_PROBE_CACHE_STALE_TIME,
          })
          if (!providerProbe.installed || !providerProbe.path) {
            throw new Error(translate(localeRef.current, 'errors.provider_not_installed', {
              provider: providerName(currentSession.provider),
            }))
          }
          startup = { probe: providerProbe, settings }
        }

        session = await materializeWorktree(
          client,
          session,
          project,
          prompt || attachments[0]?.name || 'task',
        )
        const turnCount = session.turns.at(-1)?.turn_count
        if (turnCount !== undefined) {
          try {
            await captureTurnStart(
              client,
              sessionCwd(session, project),
              session.id,
              turnCount,
            )
          } catch (error) {
            toast.error(translate(localeRef.current, 'errors.capture_pre_turn_checkpoint', {
              error: errorMessage(error),
            }))
          }
        }
        session = await persistOrdered(session)
        cacheSession(session)
      } catch (error) {
        cacheSession(currentSession)
        throw error
      }

      try {
        if (!runtime) {
          const runtimeId = crypto.randomUUID()
          runtime = subscribe(session, runtimeId)
          const response = await client.request(
            {
              type: 'start',
              options: {
                provider: session.provider,
                binary: startup!.probe.path!,
                cwd: sessionCwd(session, project),
                mode: session.runtime_mode,
                interactionMode: session.interaction_mode,
                model: session.model ?? null,
                reasoningEffort: session.reasoning_effort ?? null,
                serviceTier: session.service_tier ?? null,
                contextWindow: session.context_window ?? null,
                agentPreset: session.agent_preset ?? null,
                computerUseEnabled: false,
                providerCursor: session.provider_cursor as never,
              },
            },
            session.id,
            runtimeId,
          )
          if (response.type !== 'started') {
            throw new Error(translate(localeRef.current, 'errors.unexpected_daemon_response', {
              expected: 'started',
              actual: response.type,
            }))
          }
          runtime.supportsSteer = response.supportsSteer
          runtime.starting = false
          setRuntimes((current) => ({
            ...current,
            [session.id]: publicRuntime(runtime!),
          }))
        }
        await client.request(
          { type: 'prompt', prompt: providerPrompt },
          session.id,
          runtime.runtimeId,
        )
      } catch (error) {
        removeRuntime(session.id)
        const failed = reduceRuntimeEvent(
          session,
          syntheticEvent(session.id, runtime?.runtimeId ?? '', 'turnFinished', {
            success: false,
            summary: errorMessage(error),
          }),
        ).session
        cacheSession(failed)
        finishSettledTurn(failed)
        throw error
      }
    },
    [
      client,
      config,
      phase,
      queryClient,
      cacheSession,
      attachSession,
      subscribe,
      removeRuntime,
      persistOrdered,
      finishSettledTurn,
    ],
  )

  sendPromptRef.current = sendPrompt

  const steerPrompt = useCallback<RuntimeContextValue['steerPrompt']>(
    async (session, rawPrompt, attachments = [], providerPromptOverride) => {
      if (!client || phase !== 'connected') {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      const prompt = rawPrompt.trim()
      if (!prompt && attachments.length === 0) return
      const providerPrompt = providerPromptOverride === undefined
        ? [
            prompt,
            attachments.map((attachment) => `@${attachment.mention}`).join(' '),
          ].filter(Boolean).join(' ')
        : providerPromptOverride.trim()
      const runtime = entries.current.get(session.id)
      if (
        !runtime ||
        !runtime.supportsSteer ||
        session.status === 'connecting' ||
        session.status === 'idle' ||
        session.status === 'failed'
      ) {
        await sendPrompt(session, prompt, attachments, providerPrompt)
        return
      }
      const pending = pendingSteers.current.get(session.id) ?? []
      pending.push({ providerPrompt, displayContent: prompt, attachments })
      pendingSteers.current.set(session.id, pending)
      await client.request({ type: 'steer', prompt: providerPrompt }, session.id, runtime.runtimeId)
    },
    [client, phase, sendPrompt],
  )

  const removeQueuedMessage = useCallback(
    async (sessionId: string, messageId: string) => {
      if (!config) throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      const key = daemonKeys.session(config.address, sessionId)
      const session = queryClient.getQueryData<AgentSession>(key)
      if (!session) throw new Error(translate(localeRef.current, 'errors.task_not_loaded'))
      const next = {
        ...session,
        queued_messages: (session.queued_messages ?? []).filter((message) => message.id !== messageId),
      }
      cacheSession(next)
      await persistOrdered(next)
    },
    [config, queryClient, cacheSession, persistOrdered],
  )

  const cancel = useCallback(
    async (sessionId: string) => {
      if (!client) throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      const runtime = entries.current.get(sessionId)
      if (!runtime) throw new Error(translate(localeRef.current, 'errors.no_live_runtime'))
      await client.request({ type: 'cancel' }, sessionId, runtime.runtimeId)
    },
    [client],
  )

  const closeSession = useCallback(
    async (sessionId: string) => {
      if (!client || phase !== 'connected') {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      const runtime = entries.current.get(sessionId)
      if (runtime) {
        await client.request({ type: 'closeSession' }, sessionId, runtime.runtimeId)
        removeRuntime(sessionId)
      }
      const pendingPersistence = persistenceTails.current.get(sessionId)
      if (pendingPersistence) await pendingPersistence.catch(() => undefined)
      const pendingCheckpoint = checkpointCaptures.current.get(sessionId)
      checkpointCaptures.current.delete(sessionId)
      if (pendingCheckpoint) await pendingCheckpoint.catch(() => undefined)
      persistenceTails.current.delete(sessionId)
      saveGenerations.current.delete(sessionId)
      pendingSteers.current.delete(sessionId)
      setPermissions((current) => removeRecordKey(current, sessionId))
      setUserInputs((current) => removeRecordKey(current, sessionId))
      setBackgroundWork((current) => removeRecordKey(current, sessionId))
    },
    [client, phase, removeRuntime],
  )

  const respond = useCallback(
    async (sessionId: string, requestId: string, optionId: string) => {
      if (!client) throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      const runtime = entries.current.get(sessionId)
      if (!runtime) throw new Error(translate(localeRef.current, 'errors.no_live_runtime'))
      await client.request(
        { type: 'respond', requestId, optionId },
        sessionId,
        runtime.runtimeId,
      )
      setPermissions((current) => ({ ...current, [sessionId]: undefined }))
      const key = config && daemonKeys.session(config.address, sessionId)
      if (key) {
        const session = queryClient.getQueryData<AgentSession>(key)
        if (session) cacheSession({ ...session, status: 'working' })
      }
    },
    [client, config, queryClient, cacheSession],
  )

  const respondUserInput = useCallback(
    async (sessionId: string, requestId: string, answers: UserInputAnswer[]) => {
      if (!client) throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      const runtime = entries.current.get(sessionId)
      if (!runtime) throw new Error(translate(localeRef.current, 'errors.no_live_runtime'))
      await client.request(
        { type: 'respondUserInput', requestId, answers },
        sessionId,
        runtime.runtimeId,
      )
      setUserInputs((current) => ({ ...current, [sessionId]: undefined }))
      const key = config && daemonKeys.session(config.address, sessionId)
      if (key) {
        const session = queryClient.getQueryData<AgentSession>(key)
        if (session) cacheSession({ ...session, status: 'working' })
      }
    },
    [client, config, queryClient, cacheSession],
  )

  const refreshBackgroundWork = useCallback(
    async (sessionId: string) => {
      if (!client || phase !== 'connected') return
      const runtime = entries.current.get(sessionId)
      if (!runtime) return
      await client.notify(
        { type: 'refreshBackgroundWork' },
        sessionId,
        runtime.runtimeId,
      )
    },
    [client, phase],
  )

  const stopBackgroundWork = useCallback(
    async (sessionId: string, item: BackgroundWorkItem) => {
      if (!client || phase !== 'connected' || !item.controlId) return
      const runtime = entries.current.get(sessionId)
      if (!runtime) {
        setBackgroundWork((current) => ({
          ...current,
          [sessionId]: markBackgroundWorkLost(current[sessionId] ?? []),
        }))
        return
      }
      setBackgroundWork((current) => ({
        ...current,
        [sessionId]: reduceBackgroundWork(current[sessionId] ?? [], {
          type: 'stopRequested',
          key: item.key,
        }),
      }))
      try {
        await client.notify(
          {
            type: 'stopBackgroundWork',
            key: item.key as never,
            controlId: item.controlId,
          },
          sessionId,
          runtime.runtimeId,
        )
      } catch (error) {
        setBackgroundWork((current) => ({
          ...current,
          [sessionId]: reduceBackgroundWork(current[sessionId] ?? [], {
            type: 'stopFailed',
            key: item.key,
            message: errorMessage(error),
          }),
        }))
        throw error
      }
    },
    [client, phase],
  )

  const forkSessionFromResponse = useCallback(
    async (session: AgentSession, turnCount: number) => {
      if (!client || !config || phase !== 'connected') {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      if (responseForksInFlight.current.has(session.id)) {
        throw new Error(translate(localeRef.current, 'errors.task_already_forking'))
      }
      responseForksInFlight.current.set(session.id, turnCount)
      setResponseForks((current) => ({ ...current, [session.id]: turnCount }))
      try {
        const runtime = entries.current.get(session.id)
        const response = await client.request(
          { type: 'forkSessionFromResponse', turnCount },
          session.id,
          runtime?.runtimeId,
        )
        if (response.type !== 'sessionForked') {
          throw new Error(translate(localeRef.current, 'errors.unexpected_daemon_response', {
            expected: 'sessionForked',
            actual: response.type,
          }))
        }
        cacheSession(response.session)
        await queryClient.invalidateQueries({
          queryKey: daemonKeys.taskState(config.address),
        })
        return {
          session: response.session,
          checkpointWarning: response.checkpointWarning,
        }
      } finally {
        responseForksInFlight.current.delete(session.id)
        setResponseForks((current) => removeRecordKey(current, session.id))
      }
    },
    [client, config, phase, queryClient, cacheSession],
  )

  const rewindSessionToMessage = useCallback(
    async (
      session: AgentSession,
      turnCount: number,
      prompt: string,
      attachments: MessageAttachment[] = [],
    ) => {
      if (!client || !config || phase !== 'connected') {
        throw new Error(translate(localeRef.current, 'errors.daemon_disconnected'))
      }
      if (messageRewindsInFlight.current.has(session.id)) {
        throw new Error(translate(localeRef.current, 'errors.task_already_rewinding'))
      }
      messageRewindsInFlight.current.set(session.id, turnCount)
      setMessageRewinds((current) => ({ ...current, [session.id]: turnCount }))
      try {
        const pendingPersistence = persistenceTails.current.get(session.id)
        if (pendingPersistence) await pendingPersistence
        const runtime = entries.current.get(session.id)
        const response = await client.request(
          { type: 'rewindSessionToMessage', turnCount },
          session.id,
          runtime?.runtimeId,
        )
        if (response.type !== 'sessionRewound') {
          throw new Error(translate(localeRef.current, 'errors.unexpected_daemon_response', {
            expected: 'sessionRewound',
            actual: response.type,
          }))
        }

        removeRuntime(session.id)
        cacheSession(response.session)
        void Promise.all([
          queryClient.invalidateQueries({
            queryKey: daemonKeys.sessionTurnRefsRoot(config.address),
          }),
          queryClient.invalidateQueries({
            queryKey: ['daemon', config.address, 'workspace'],
          }),
          queryClient.invalidateQueries({
            queryKey: ['daemon', config.address, 'workspace-tree'],
          }),
          queryClient.invalidateQueries({
            queryKey: ['daemon', config.address, 'workspace-diff'],
          }),
        ]).catch(() => {})
        await sendPrompt(response.session, prompt, attachments)
        return {
          session: response.session,
          cleanupWarning: response.cleanupWarning,
        }
      } finally {
        messageRewindsInFlight.current.delete(session.id)
        setMessageRewinds((current) => removeRecordKey(current, session.id))
      }
    },
    [client, config, phase, queryClient, cacheSession, removeRuntime, sendPrompt],
  )

  useEffect(() => {
    const current = entries.current
    for (const timer of projectionPersistTimers.current.values()) {
      window.clearTimeout(timer)
    }
    projectionPersistTimers.current.clear()
    attachRequests.current.clear()
    persistenceTails.current.clear()
    checkpointCaptures.current.clear()
    saveGenerations.current.clear()
    pendingSteers.current.clear()
    responseForksInFlight.current.clear()
    messageRewindsInFlight.current.clear()
    setRuntimes({})
    setPermissions({})
    setUserInputs({})
    setBackgroundWork({})
    setResponseForks({})
    setMessageRewinds({})
    return () => {
      for (const timer of projectionPersistTimers.current.values()) {
        window.clearTimeout(timer)
      }
      projectionPersistTimers.current.clear()
      for (const entry of current.values()) entry.unsubscribe()
      current.clear()
    }
  }, [client])

  const value: RuntimeContextValue = {
    runtimes,
    permissions,
    userInputs,
    backgroundWork,
    responseForks,
    messageRewinds,
    attachSession,
    sendPrompt,
    steerPrompt,
    cancel,
    closeSession,
    removeQueuedMessage,
    respond,
    respondUserInput,
    refreshBackgroundWork,
    stopBackgroundWork,
    forkSessionFromResponse,
    rewindSessionToMessage,
    saveSession,
  }

  return <RuntimeContext.Provider value={value}>{children}</RuntimeContext.Provider>
}

function removeRecordKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  if (!(key in record)) return record
  const next = { ...record }
  delete next[key]
  return next
}

function queueSubmission(
  session: AgentSession,
  displayContent: string,
  providerPrompt: string,
  attachments: MessageAttachment[],
): AgentSession {
  return {
    ...session,
    updated_at: Math.floor(Date.now() / 1_000),
    queued_messages: [
      ...(session.queued_messages ?? []),
      {
        id: crypto.randomUUID(),
        content: providerPrompt,
        display_content: attachments.length || providerPrompt !== displayContent
          ? displayContent
          : null,
        attachments,
        created_at: Math.floor(Date.now() / 1_000),
      },
    ],
  }
}

export function useRuntime() {
  const context = useContext(RuntimeContext)
  if (!context) throw new Error('useRuntime must be used inside RuntimeProvider')
  return context
}

function mergeSessionSummary(previous: AgentSession, next: AgentSession): AgentSession {
  return {
    ...previous,
    title: next.title,
    auto_title: next.auto_title,
    project_id: next.project_id,
    workspace: next.workspace,
    provider: next.provider,
    model: next.model,
    runtime_mode: next.runtime_mode,
    interaction_mode: next.interaction_mode,
    reasoning_effort: next.reasoning_effort,
    service_tier: next.service_tier,
    agent_preset: next.agent_preset,
    status: next.status,
    updated_at: next.updated_at,
    last_reply_at: next.last_reply_at,
    provider_cursor: next.provider_cursor,
    context_usage: next.context_usage,
    runtime_event_cursor: next.runtime_event_cursor,
  }
}

function runtimeEventAlreadyApplied(session: AgentSession, event: SequencedEvent) {
  const cursor = session.runtime_event_cursor
  return Boolean(
    cursor
      && cursor.runtime_id === event.runtimeId
      && cursor.epoch === event.epoch
      && cursor.sequence >= event.sequence,
  )
}

function publicRuntime(runtime: RuntimeEntry): RuntimeSummary {
  return {
    runtimeId: runtime.runtimeId,
    supportsSteer: runtime.supportsSteer,
    starting: runtime.starting,
  }
}

function syntheticEvent(
  sessionId: string,
  runtimeId: string,
  kind: string,
  payload: unknown,
): SequencedEvent {
  return {
    sessionId,
    runtimeId,
    epoch: 'local',
    sequence: 0,
    event: { kind, payload: payload as never },
  }
}

function providerName(provider: AgentSession['provider']) {
  return (
    {
      amp: 'Amp',
      claude: 'Claude Code',
      codex: 'Codex',
      cursor: 'Cursor Agent',
      deepSeek: 'DeepSeek Harness',
      openCode: 'OpenCode',
      grok: 'Grok',
      pi: 'Pi',
    } as const
  )[provider]
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

const MAX_BACKGROUND_OUTPUT_CHARS = 512 * 1024
const MAX_SETTLED_BACKGROUND_ITEMS = 24

function decodeBackgroundWorkEvent(payload: unknown): BackgroundWorkEvent | null {
  const value = asObject(payload)
  if (!value || typeof value.type !== 'string') return null
  if (value.type === 'outputDelta') {
    const key = backgroundKey(value.key)
    return key && typeof value.delta === 'string'
      ? { type: 'outputDelta', key, delta: value.delta }
      : null
  }
  if (value.type === 'stopFailed') {
    const key = backgroundKey(value.key)
    return key && typeof value.message === 'string'
      ? { type: 'stopFailed', key, message: value.message }
      : null
  }
  if (value.type === 'upsert') {
    const item = backgroundItem(value.item ?? value.data ?? value['0'] ?? value)
    return item ? { type: 'upsert', item } : null
  }
  if (value.type === 'reconcileProcesses' || value.type === 'reconcileLive') {
    const rawItems = value.items ?? value.data ?? value['0']
    const items = Array.isArray(rawItems)
      ? rawItems.map(backgroundItem).filter((item): item is BackgroundWorkItem => Boolean(item))
      : []
    return { type: value.type, items }
  }
  if (value.type === 'stopRequested') {
    const key = backgroundKey(value.key ?? value.data ?? value['0'] ?? value)
    return key ? { type: 'stopRequested', key } : null
  }
  return null
}

function reduceBackgroundWork(
  current: BackgroundWorkItem[],
  event: BackgroundWorkEvent,
): BackgroundWorkItem[] {
  let next = current.map((item) => ({ ...item, key: { ...item.key } }))
  if (event.type === 'upsert') {
    next = upsertBackgroundItem(next, event.item)
  } else if (event.type === 'outputDelta') {
    next = next.map((item) => sameBackgroundKey(item.key, event.key)
      ? {
          ...item,
          output: boundBackgroundOutput(`${item.output ?? ''}${event.delta}`),
          outputTruncated: item.outputTruncated
            || `${item.output ?? ''}${event.delta}`.length > MAX_BACKGROUND_OUTPUT_CHARS,
          updatedAtMs: Date.now(),
        }
      : item)
  } else if (event.type === 'reconcileProcesses') {
    next = reconcileBackgroundItems(next, event.items, false)
  } else if (event.type === 'reconcileLive') {
    next = reconcileBackgroundItems(next, event.items, true)
  } else if (event.type === 'stopRequested') {
    next = next.map((item) => sameBackgroundKey(item.key, event.key)
      ? { ...item, status: 'stopping', updatedAtMs: Date.now() }
      : item)
  } else {
    next = next.map((item) => {
      if (!sameBackgroundKey(item.key, event.key) || !isLiveBackgroundStatus(item.status)) {
        return item
      }
      return {
        ...item,
        detail: event.message,
        status: item.key.kind === 'monitor' ? 'monitoring' : 'running',
        updatedAtMs: Date.now(),
      }
    })
  }
  return trimSettledBackgroundItems(next)
}

function upsertBackgroundItem(
  current: BackgroundWorkItem[],
  incoming: BackgroundWorkItem,
): BackgroundWorkItem[] {
  const index = current.findIndex((item) => sameBackgroundKey(item.key, incoming.key))
  const existing = index >= 0 ? current[index] : undefined
  if (
    incoming.key.kind !== 'subagent'
    && !incoming.background
    && !existing?.background
    && !isLiveBackgroundStatus(incoming.status)
  ) {
    return existing ? current.filter((_, itemIndex) => itemIndex !== index) : current
  }
  const output = incoming.output == null
    ? existing?.output ?? null
    : boundBackgroundOutput(incoming.output)
  const merged: BackgroundWorkItem = existing
    ? {
        ...existing,
        ...incoming,
        title: incoming.title || existing.title,
        detail: incoming.detail ?? existing.detail,
        command: incoming.command ?? existing.command,
        cwd: incoming.cwd ?? existing.cwd,
        output,
        outputTruncated: incoming.output == null
          ? existing.outputTruncated
          : incoming.outputTruncated || incoming.output.length > MAX_BACKGROUND_OUTPUT_CHARS,
        startedAtMs: Math.min(existing.startedAtMs, incoming.startedAtMs),
        updatedAtMs: Math.max(existing.updatedAtMs, incoming.updatedAtMs),
        durationMs: incoming.durationMs ?? existing.durationMs,
        exitCode: incoming.exitCode ?? existing.exitCode,
        background: existing.background || incoming.background,
        canStop: isLiveBackgroundStatus(incoming.status)
          ? existing.canStop || incoming.canStop
          : false,
        controlId: incoming.controlId ?? existing.controlId,
        originActivityId: incoming.originActivityId ?? existing.originActivityId,
        role: incoming.role ?? existing.role,
        model: incoming.model ?? existing.model,
        parentId: incoming.parentId ?? existing.parentId,
        status: existing.status === 'stopping' && isStoppableBackgroundStatus(incoming.status)
          ? 'stopping'
          : incoming.status,
      }
    : {
        ...incoming,
        output,
        outputTruncated: incoming.outputTruncated
          || (incoming.output?.length ?? 0) > MAX_BACKGROUND_OUTPUT_CHARS,
      }
  if (index < 0) return [...current, merged]
  const next = [...current]
  next[index] = merged
  return next
}

function reconcileBackgroundItems(
  current: BackgroundWorkItem[],
  incoming: BackgroundWorkItem[],
  allKinds: boolean,
): BackgroundWorkItem[] {
  const present = new Set(incoming.map((item) => backgroundKeyId(item.key)))
  let next = current.map((item) => {
    const includedKind = allKinds || item.key.kind !== 'subagent'
    if (
      includedKind
      && item.background
      && isLiveBackgroundStatus(item.status)
      && !present.has(backgroundKeyId(item.key))
    ) {
      return { ...item, status: 'lost' as const, canStop: false, updatedAtMs: Date.now() }
    }
    return item
  })
  for (const item of incoming) next = upsertBackgroundItem(next, item)
  return next
}

function markBackgroundWorkLost(current: BackgroundWorkItem[]): BackgroundWorkItem[] {
  return current.map((item) => isLiveBackgroundStatus(item.status)
    ? { ...item, status: 'lost', canStop: false, updatedAtMs: Date.now() }
    : item)
}

function trimSettledBackgroundItems(items: BackgroundWorkItem[]): BackgroundWorkItem[] {
  const settled = items.filter((item) => !isLiveBackgroundStatus(item.status))
  if (settled.length <= MAX_SETTLED_BACKGROUND_ITEMS) return items
  const remove = new Set(
    settled
      .slice(0, settled.length - MAX_SETTLED_BACKGROUND_ITEMS)
      .map((item) => backgroundKeyId(item.key)),
  )
  return items.filter((item) => !remove.has(backgroundKeyId(item.key)))
}

function backgroundItem(value: unknown): BackgroundWorkItem | null {
  const item = asObject(value)
  const key = backgroundKey(item?.key)
  if (
    !item
    || !key
    || typeof item.title !== 'string'
    || !isBackgroundStatus(item.status)
  ) return null
  return item as unknown as BackgroundWorkItem
}

function backgroundKey(value: unknown): BackgroundWorkKey | null {
  const key = asObject(value)
  if (!key || !isBackgroundKind(key.kind) || typeof key.providerId !== 'string') return null
  return { kind: key.kind, providerId: key.providerId }
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value != null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null
}

function backgroundKeyId(key: BackgroundWorkKey) {
  return `${key.kind}:${key.providerId}`
}

function sameBackgroundKey(left: BackgroundWorkKey, right: BackgroundWorkKey) {
  return left.kind === right.kind && left.providerId === right.providerId
}

function isBackgroundKind(value: unknown): value is BackgroundWorkKind {
  return value === 'process' || value === 'monitor' || value === 'subagent'
}

function isBackgroundStatus(value: unknown): value is BackgroundWorkStatus {
  return [
    'starting',
    'running',
    'monitoring',
    'stopping',
    'completed',
    'failed',
    'stopped',
    'lost',
  ].includes(String(value))
}

function isLiveBackgroundStatus(status: BackgroundWorkStatus) {
  return status === 'starting'
    || status === 'running'
    || status === 'monitoring'
    || status === 'stopping'
}

function isStoppableBackgroundStatus(status: BackgroundWorkStatus) {
  return status === 'starting' || status === 'running' || status === 'monitoring'
}

function boundBackgroundOutput(output: string) {
  return output.length > MAX_BACKGROUND_OUTPUT_CHARS
    ? output.slice(output.length - MAX_BACKGROUND_OUTPUT_CHARS)
    : output
}
