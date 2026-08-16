import { useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import type {
  Automation,
  AutomationRun,
  InteractionMode,
  NotificationTrigger,
  OverlapPolicy,
  Project,
  ProviderKind,
  RunOutcome,
  RuntimeMode,
  Schedule,
  TimeOfDay,
  Weekday,
} from '@waku/client'
import { useEffect, useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import { ConnectionPanel } from '@/components/connection-panel'
import { StartupScreen } from '@/components/startup-screen'
import { Transcript } from '@/components/transcript'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import {
  ProviderIcon,
  PROVIDERS,
  WakuIcon,
  type WakuIconName,
} from '@/components/waku-icon'
import { useSession, useTaskState } from '@/hooks/use-daemon-data'
import { useDocumentTitle } from '@/hooks/use-document-title'
import {
  automationFromForm,
  cloneAutomation,
  createAutomation,
  createDeleteConfirmation,
  scheduleWithKind,
  type AutomationDeleteConfirmation,
} from '@/lib/automations-authoring'
import {
  automationRunTimeLabel,
  automationScheduleSummary,
  type AutomationTranslator,
} from '@/lib/automations-presentation'
import {
  applyAutomationChanges,
  daemonKeys,
  runAutomation,
  type TaskState,
  unixTime,
} from '@/lib/daemon-api'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'

const WEEKDAYS: Weekday[] = [
  'monday',
  'tuesday',
  'wednesday',
  'thursday',
  'friday',
  'saturday',
  'sunday',
]
const RUNTIME_MODES: RuntimeMode[] = [
  'plan',
  'ask',
  'autoAcceptEdits',
  'auto',
  'fullAccess',
]
const INTERACTION_MODES: InteractionMode[] = ['build', 'plan']
const OVERLAP_POLICIES: OverlapPolicy[] = ['skip', 'queue', 'concurrent']
const NOTIFICATION_TRIGGERS: NotificationTrigger[] = [
  'always',
  'onSuccess',
  'onFailure',
]

/**
 * Web surface for automation authoring, schedules, run history, and transcripts.
 *
 * @returns The connected automation catalog and selected automation workflow.
 */
export function AutomationsView() {
  const { locale, t } = useI18n()
  const { client, config, phase } = useDaemon()
  const navigate = useNavigate({ from: '/automations' })
  const queryClient = useQueryClient()
  const taskState = useTaskState()
  const [selectedId, setSelectedId] = useState<string | undefined>()
  const [editing, setEditing] = useState<Automation | undefined>()
  const [transcriptSessionId, setTranscriptSessionId] = useState<
    string | undefined
  >()
  const [deleteConfirmation, setDeleteConfirmation] =
    useState<AutomationDeleteConfirmation>()
  const [pending, setPending] = useState<string | undefined>()
  const automations = taskState.data?.automations ?? []
  const selected = automations.find(
    (automation) => automation.id === selectedId,
  )
  const deleteTarget = automations.find(
    (automation) => automation.id === deleteConfirmation?.automationId,
  )
  const transcript = useSession(transcriptSessionId)

  useDocumentTitle(t('automations.title'))

  useEffect(() => {
    if (!automations.length) {
      setSelectedId(undefined)
      setTranscriptSessionId(undefined)
      return
    }
    if (
      !selectedId ||
      !automations.some((automation) => automation.id === selectedId)
    ) {
      setSelectedId(automations[0]!.id)
      setTranscriptSessionId(undefined)
    }
  }, [automations, selectedId])

  if (phase !== 'connected')
    return <ConnectionPanel title={t('automations.title')} />
  if (!taskState.data) {
    return (
      <StartupScreen
        error={taskState.error ? errorMessage(taskState.error) : undefined}
        onRetry={() => void taskState.refetch()}
      />
    )
  }
  if (!client || !config)
    return <ConnectionPanel title={t('automations.title')} />

  const stateKey = daemonKeys.taskState(config.address)
  const selectedRun = selected?.history?.find(
    (run) => run.session_id === transcriptSessionId,
  )

  function cacheCatalog(nextAutomations: Automation[]) {
    queryClient.setQueryData<TaskState>(
      stateKey,
      (current) =>
        current && {
          ...current,
          automations: nextAutomations,
        },
    )
  }

  async function upsertAutomation(automation: Automation) {
    const normalized = automationFromForm(automation, unixTime())
    if (!normalized.name) throw new Error(t('automations.name_required'))
    if (!normalized.prompt) throw new Error(t('automations.prompt_required'))
    const next = await applyAutomationChanges(client!, [
      { kind: 'upsert', automation: normalized },
    ])
    cacheCatalog(next)
    return normalized
  }

  async function saveAutomation(automation: Automation) {
    setPending(`save:${automation.id}`)
    try {
      const saved = await upsertAutomation(automation)
      setSelectedId(saved.id)
      setEditing(undefined)
      toast.success(t('automations.saved'))
    } catch (error) {
      toast.error(errorMessage(error))
    } finally {
      setPending(undefined)
    }
  }

  async function toggleAutomation(automation: Automation) {
    setPending(`toggle:${automation.id}`)
    try {
      await upsertAutomation({ ...automation, enabled: !automation.enabled })
    } catch (error) {
      toast.error(errorMessage(error))
    } finally {
      setPending(undefined)
    }
  }

  async function startRun(automation: Automation) {
    setPending(`run:${automation.id}`)
    try {
      const response = await runAutomation(client!, automation.id)
      queryClient.setQueryData<TaskState>(
        stateKey,
        (current) =>
          current && {
            ...current,
            automations: current.automations.map((candidate) =>
              candidate.id === response.automation.id
                ? response.automation
                : candidate,
            ),
            sessions: current.sessions.some(
              (session) => session.id === response.session.id,
            )
              ? current.sessions.map((session) =>
                  session.id === response.session.id
                    ? response.session
                    : session,
                )
              : [...current.sessions, response.session],
          },
      )
      await queryClient.invalidateQueries({ queryKey: stateKey })
      toast.success(t('automations.run_started'))
    } catch (error) {
      toast.error(errorMessage(error))
    } finally {
      setPending(undefined)
    }
  }

  async function deleteAutomation() {
    if (!deleteConfirmation || !deleteTarget) return
    setPending(`delete:${deleteTarget.id}`)
    try {
      const next = await applyAutomationChanges(client!, [
        {
          kind: 'remove',
          automation_id: deleteTarget.id,
          cascade_sessions: deleteConfirmation.cascadeSessions,
        },
      ])
      queryClient.setQueryData<TaskState>(
        stateKey,
        (current) =>
          current && {
            ...current,
            automations: next,
            sessions: deleteConfirmation.cascadeSessions
              ? current.sessions.filter(
                  (session) =>
                    session.originating_automation !== deleteTarget.id,
                )
              : current.sessions,
          },
      )
      setDeleteConfirmation(undefined)
      setEditing(undefined)
      setTranscriptSessionId(undefined)
      toast.success(t('automations.deleted'))
    } catch (error) {
      toast.error(errorMessage(error))
    } finally {
      setPending(undefined)
    }
  }

  function beginNewAutomation() {
    setEditing(createAutomation(crypto.randomUUID(), unixTime()))
    setTranscriptSessionId(undefined)
  }

  return (
    <main className="flex h-dvh min-w-0 flex-col overflow-hidden bg-background">
      <header className="flex min-h-12 shrink-0 items-center justify-between gap-3 border-b px-4 py-2 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <Button
            aria-label={t('settings.back')}
            size="icon-sm"
            variant="ghost"
            onClick={() =>
              void navigate({ to: '/', search: { session: undefined } })
            }
          >
            <WakuIcon name="arrowLeft" />
          </Button>
          <div className="min-w-0">
            <h1 className="truncate text-[18px] font-medium">
              {t('automations.title')}
            </h1>
            <p className="truncate text-[11.5px] text-[var(--text-tertiary)]">
              {t('automations.web_description')}
            </p>
          </div>
        </div>
        <Button size="sm" onClick={beginNewAutomation}>
          <WakuIcon name="plus" />
          {t('automations.new')}
        </Button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-4 sm:px-6 sm:py-6">
        <div className="mx-auto flex w-full max-w-[1180px] flex-col gap-4">
          <ClientNotice connected={phase === 'connected'} t={t} />
          <div className="grid min-w-0 grid-cols-1 gap-4 xl:grid-cols-[minmax(280px,360px)_minmax(0,1fr)]">
            <AutomationList
              automations={automations}
              selectedId={selectedId}
              t={t}
              onNew={beginNewAutomation}
              onSelect={(id) => {
                setSelectedId(id)
                setEditing(undefined)
                setTranscriptSessionId(undefined)
              }}
            />
            <section className="min-w-0 rounded-[13px] border bg-[var(--raised)] p-4 sm:p-5">
              {editing ? (
                <AutomationForm
                  automation={editing}
                  pending={pending === `save:${editing.id}`}
                  projects={taskState.data.projects}
                  t={t}
                  onCancel={() => setEditing(undefined)}
                  onSave={(automation) => void saveAutomation(automation)}
                />
              ) : selected ? (
                <AutomationDetails
                  automation={selected}
                  locale={locale}
                  pending={pending}
                  projects={taskState.data.projects}
                  selectedRun={selectedRun}
                  transcript={transcript.data}
                  transcriptError={transcript.error}
                  transcriptPending={transcript.isPending}
                  transcriptSessionId={transcriptSessionId}
                  t={t}
                  onDelete={() =>
                    setDeleteConfirmation(
                      createDeleteConfirmation(
                        selected.id,
                        taskState.data!.sessions,
                      ),
                    )
                  }
                  onEdit={() => setEditing(cloneAutomation(selected))}
                  onOpenRun={setTranscriptSessionId}
                  onOpenTask={(sessionId) =>
                    void navigate({
                      to: '/',
                      search: { session: sessionId },
                    })
                  }
                  onRun={() => void startRun(selected)}
                  onToggle={() => void toggleAutomation(selected)}
                />
              ) : (
                <EmptyDetails t={t} onNew={beginNewAutomation} />
              )}
            </section>
          </div>
        </div>
      </div>

      <DeleteAutomationDialog
        automation={deleteTarget}
        confirmation={deleteConfirmation}
        pending={pending === `delete:${deleteTarget?.id}`}
        t={t}
        onCancel={() => setDeleteConfirmation(undefined)}
        onChange={(cascadeSessions) =>
          setDeleteConfirmation(
            (current) =>
              current && {
                ...current,
                cascadeSessions,
              },
          )
        }
        onConfirm={() => void deleteAutomation()}
      />
    </main>
  )
}

function ClientNotice({
  connected,
  t,
}: {
  connected: boolean
  t: AutomationTranslator
}) {
  return (
    <div
      className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-xl border bg-[var(--raised)] px-4 py-3 text-[12px]"
      role="status"
    >
      <WakuIcon
        className={cn(
          'size-3.5',
          connected ? 'text-[var(--success)]' : 'text-[var(--warning)]',
        )}
        name={connected ? 'check' : 'alert'}
      />
      <span className="font-medium">
        {t(
          connected
            ? 'automations.client_connected'
            : 'automations.client_disconnected',
        )}
      </span>
      <span className="text-[var(--text-secondary)]">
        {t('automations.daemon_required')}
      </span>
    </div>
  )
}

function AutomationList({
  automations,
  selectedId,
  t,
  onNew,
  onSelect,
}: {
  automations: Automation[]
  selectedId: string | undefined
  t: AutomationTranslator
  onNew: () => void
  onSelect: (id: string) => void
}) {
  return (
    <section
      aria-label={t('automations.title')}
      className="min-w-0 self-start rounded-[13px] border bg-[var(--raised)] p-2 xl:sticky xl:top-0 xl:w-full"
    >
      {automations.length === 0 ? (
        <div className="flex min-h-48 flex-col items-center justify-center px-5 text-center">
          <WakuIcon className="size-7 text-ring" name="zap" />
          <h2 className="mt-3 text-[14px] font-medium">
            {t('automations.empty_title')}
          </h2>
          <p className="mt-1.5 max-w-[270px] text-[12px] leading-5 text-[var(--text-secondary)]">
            {t('automations.empty_body')}
          </p>
          <Button className="mt-4" size="sm" onClick={onNew}>
            <WakuIcon name="plus" />
            {t('automations.new')}
          </Button>
        </div>
      ) : (
        <div className="flex flex-col gap-1">
          {automations.map((automation) => (
            <button
              aria-current={selectedId === automation.id ? 'true' : undefined}
              className={cn(
                'w-full rounded-[9px] px-3 py-3 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring',
                selectedId === automation.id
                  ? 'bg-accent'
                  : 'hover:bg-accent/60',
              )}
              key={automation.id}
              type="button"
              onClick={() => onSelect(automation.id)}
            >
              <span className="flex min-w-0 items-center gap-2">
                <WakuIcon
                  className={cn(
                    'size-3.5',
                    automation.enabled
                      ? 'text-[var(--success)]'
                      : 'text-[var(--text-ghost)]',
                  )}
                  name={automation.enabled ? 'check' : 'stop'}
                />
                <span className="min-w-0 flex-1 truncate text-[13px] font-medium">
                  {automation.name}
                </span>
              </span>
              <span className="mt-1 block truncate pl-5 text-[11.5px] text-[var(--text-secondary)]">
                {automationScheduleSummary(automation.schedule, t)}
              </span>
              <span className="mt-1 block pl-5 text-[10.5px] text-[var(--text-tertiary)]">
                {automation.enabled
                  ? t('automations.enabled')
                  : t('automations.disabled')}
              </span>
            </button>
          ))}
        </div>
      )}
    </section>
  )
}

function AutomationDetails({
  automation,
  locale,
  pending,
  projects,
  selectedRun,
  transcriptSessionId,
  transcript,
  transcriptError,
  transcriptPending,
  t,
  onDelete,
  onEdit,
  onOpenRun,
  onOpenTask,
  onRun,
  onToggle,
}: {
  automation: Automation
  locale: string
  pending: string | undefined
  projects: Project[]
  selectedRun: AutomationRun | undefined
  transcriptSessionId: string | undefined
  transcript: Parameters<typeof Transcript>[0]['session'] | null | undefined
  transcriptError: Error | null
  transcriptPending: boolean
  t: AutomationTranslator
  onDelete: () => void
  onEdit: () => void
  onOpenRun: (sessionId: string) => void
  onOpenTask: (sessionId: string) => void
  onRun: () => void
  onToggle: () => void
}) {
  const history = automation.history ?? []
  const busy = Boolean(pending)
  const project = projects.find(
    (candidate) => candidate.id === automation.project_id,
  )
  return (
    <div className="flex min-w-0 flex-col gap-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="min-w-0 truncate text-[18px] font-medium">
              {automation.name}
            </h2>
            <span
              className={cn(
                'rounded-full px-2 py-0.5 text-[10px] font-medium',
                automation.enabled
                  ? 'bg-[color-mix(in_srgb,var(--success)_14%,transparent)] text-[var(--success)]'
                  : 'bg-accent text-[var(--text-tertiary)]',
              )}
            >
              {automation.enabled
                ? t('automations.enabled')
                : t('automations.disabled')}
            </span>
          </div>
          <p className="mt-1 text-[12px] text-[var(--text-secondary)]">
            {automationScheduleSummary(automation.schedule, t)}
          </p>
        </div>
        <div className="flex flex-wrap gap-1.5">
          <Button disabled={busy} size="sm" onClick={onRun}>
            <WakuIcon
              className={cn(
                pending === `run:${automation.id}` && 'animate-spin',
              )}
              name={pending === `run:${automation.id}` ? 'loaderCircle' : 'zap'}
            />
            {t('automations.run_now')}
          </Button>
          <Button disabled={busy} size="sm" variant="outline" onClick={onEdit}>
            <WakuIcon name="pencil" />
            {t('automations.edit')}
          </Button>
          <Button disabled={busy} size="sm" variant="ghost" onClick={onToggle}>
            {automation.enabled
              ? t('automations.menu_disable')
              : t('automations.menu_enable')}
          </Button>
          <Button
            aria-label={t('automations.delete')}
            disabled={busy}
            size="icon-sm"
            variant="destructive"
            onClick={onDelete}
          >
            <WakuIcon name="trash" />
          </Button>
        </div>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <InfoCard
          icon={<ProviderIcon provider={automation.agent.provider} />}
          label={t('automations.field_provider')}
          value={providerName(automation.agent.provider)}
        />
        <InfoCard
          icon={<WakuIcon name="folder" />}
          label={t('automations.field_project')}
          value={project?.name ?? t('automations.no_project')}
        />
        <InfoCard
          icon={<WakuIcon name="loaderCircle" />}
          label={t('automations.field_overlap')}
          value={overlapLabel(automation.overlap, t)}
        />
        <InfoCard
          icon={<WakuIcon name="alert" />}
          label={t('automations.field_notifications')}
          value={
            automation.notification.enabled
              ? notificationLabel(automation.notification.trigger, t)
              : t('automations.notifications_off')
          }
        />
      </div>

      <div>
        <h3 className="text-[12px] font-medium text-[var(--text-secondary)]">
          {t('automations.field_instructions')}
        </h3>
        <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap rounded-xl border bg-[var(--inset)] p-3 font-sans text-[12.5px] leading-5 text-foreground">
          {automation.prompt}
        </pre>
      </div>

      <div className="min-w-0">
        <div className="flex items-center justify-between gap-2">
          <h3 className="text-[12px] font-medium text-[var(--text-secondary)]">
            {t('automations.history')}
          </h3>
          <span className="text-[10.5px] text-[var(--text-tertiary)]">
            {history.length}
          </span>
        </div>
        {history.length === 0 ? (
          <p className="mt-2 rounded-xl border border-dashed px-3 py-4 text-[12px] text-[var(--text-tertiary)]">
            {t('automations.no_runs')}
          </p>
        ) : (
          <div className="mt-2 flex max-h-72 flex-col gap-1 overflow-y-auto">
            {history.map((run) => (
              <RunRow
                key={run.id}
                locale={locale}
                run={run}
                selected={run.session_id === transcriptSessionId}
                t={t}
                onOpen={onOpenRun}
              />
            ))}
          </div>
        )}
      </div>

      {transcriptSessionId && (
        <div className="min-w-0 border-t pt-4">
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
            <h3 className="text-[12px] font-medium text-[var(--text-secondary)]">
              {t('automations.run_transcript')}
            </h3>
            {selectedRun?.session_id && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => onOpenTask(selectedRun.session_id!)}
              >
                {t('automations.open_task')}
                <WakuIcon name="arrowRight" />
              </Button>
            )}
          </div>
          {transcriptPending ? (
            <TranscriptState>
              {t('automations.loading_transcript')}
            </TranscriptState>
          ) : transcript ? (
            <div className="h-[min(560px,70dvh)] min-h-80 overflow-hidden rounded-xl border bg-background">
              <Transcript session={transcript} />
            </div>
          ) : (
            <TranscriptState error={Boolean(transcriptError)}>
              {transcriptError
                ? errorMessage(transcriptError)
                : t('automations.transcript_unavailable')}
            </TranscriptState>
          )}
        </div>
      )}
    </div>
  )
}

function AutomationForm({
  automation: initial,
  pending,
  projects,
  t,
  onCancel,
  onSave,
}: {
  automation: Automation
  pending: boolean
  projects: Project[]
  t: AutomationTranslator
  onCancel: () => void
  onSave: (automation: Automation) => void
}) {
  const [automation, setAutomation] = useState(() => cloneAutomation(initial))
  const [monthdayText, setMonthdayText] = useState(
    initial.schedule.kind === 'monthly'
      ? initial.schedule.days.join(', ')
      : '1',
  )
  const schedule = automation.schedule
  const time =
    schedule.kind === 'daily' ||
    schedule.kind === 'weekly' ||
    schedule.kind === 'monthly'
      ? schedule.time
      : { hour: 9, minute: schedule.kind === 'hourly' ? schedule.minute : 0 }

  function updateAgent<K extends keyof Automation['agent']>(
    key: K,
    value: Automation['agent'][K],
  ) {
    setAutomation((current) => ({
      ...current,
      agent: { ...current.agent, [key]: value },
    }))
  }

  function updateTime(key: keyof TimeOfDay, rawValue: string) {
    const maximum = key === 'hour' ? 23 : 59
    const value = Math.max(
      0,
      Math.min(maximum, Number.parseInt(rawValue, 10) || 0),
    )
    setAutomation((current) => {
      if (current.schedule.kind === 'hourly' && key === 'minute') {
        return { ...current, schedule: { ...current.schedule, minute: value } }
      }
      if (
        current.schedule.kind === 'daily' ||
        current.schedule.kind === 'weekly' ||
        current.schedule.kind === 'monthly'
      ) {
        return {
          ...current,
          schedule: {
            ...current.schedule,
            time: { ...current.schedule.time, [key]: value },
          },
        }
      }
      return current
    })
  }

  function toggleWeekday(day: Weekday) {
    setAutomation((current) => {
      if (current.schedule.kind !== 'weekly') return current
      const weekdays = current.schedule.weekdays.includes(day)
        ? current.schedule.weekdays.filter((candidate) => candidate !== day)
        : [...current.schedule.weekdays, day]
      return { ...current, schedule: { ...current.schedule, weekdays } }
    })
  }

  function updateMonthdays(value: string) {
    setMonthdayText(value)
    const days = value
      .split(',')
      .map((part) => Number.parseInt(part.trim(), 10))
      .filter(Number.isFinite)
    setAutomation((current) =>
      current.schedule.kind === 'monthly'
        ? { ...current, schedule: { ...current.schedule, days } }
        : current,
    )
  }

  function updateProject(value: string) {
    const projectId = value === 'none' ? null : value
    setAutomation((current) => ({
      ...current,
      project_id: projectId,
      workspace:
        projectId && current.workspace?.kind === 'newWorktree'
          ? current.workspace
          : { kind: 'local' },
    }))
  }

  return (
    <form
      className="flex min-w-0 flex-col gap-5"
      onSubmit={(event) => {
        event.preventDefault()
        onSave(automation)
      }}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-[18px] font-medium">
            {initial.name
              ? t('automations.edit_title')
              : t('automations.create_title')}
          </h2>
          <p className="mt-1 text-[12px] text-[var(--text-secondary)]">
            {t('automations.authoring_description')}
          </p>
        </div>
        <div className="flex gap-1.5">
          <Button
            disabled={pending}
            type="button"
            variant="ghost"
            onClick={onCancel}
          >
            {t('automations.cancel')}
          </Button>
          <Button disabled={pending} type="submit">
            <WakuIcon
              className={cn(pending && 'animate-spin')}
              name={pending ? 'loaderCircle' : 'check'}
            />
            {t('automations.save')}
          </Button>
        </div>
      </div>

      <Field
        description={t('automations.field_name_description')}
        htmlFor="automation-name"
        label={t('automations.field_name')}
      >
        <Input
          autoFocus
          id="automation-name"
          required
          value={automation.name}
          onChange={(event) =>
            setAutomation((current) => ({
              ...current,
              name: event.target.value,
            }))
          }
        />
      </Field>
      <Field
        description={t('automations.field_instructions_description')}
        htmlFor="automation-prompt"
        label={t('automations.field_instructions')}
      >
        <Textarea
          className="min-h-32 resize-y"
          id="automation-prompt"
          placeholder={t('automations.prompt_placeholder')}
          required
          value={automation.prompt}
          onChange={(event) =>
            setAutomation((current) => ({
              ...current,
              prompt: event.target.value,
            }))
          }
        />
      </Field>

      <div className="grid gap-4 sm:grid-cols-2">
        <Field
          htmlFor="automation-provider"
          label={t('automations.field_provider')}
        >
          <Select
            id="automation-provider"
            options={PROVIDERS.map((provider) => ({
              value: provider.id,
              label: provider.name,
            }))}
            value={automation.agent.provider}
            onChange={(value) => updateAgent('provider', value as ProviderKind)}
          />
        </Field>
        <Field
          description={t('automations.model_description')}
          htmlFor="automation-model"
          label={t('automations.field_model')}
        >
          <Input
            id="automation-model"
            placeholder={t('automations.model_default')}
            value={automation.agent.model ?? ''}
            onChange={(event) =>
              updateAgent('model', event.target.value || null)
            }
          />
        </Field>
        <Field
          htmlFor="automation-effort"
          label={t('automations.field_effort')}
        >
          <Input
            id="automation-effort"
            placeholder={t('automations.effort_default')}
            value={automation.agent.reasoning_effort ?? ''}
            onChange={(event) =>
              updateAgent('reasoning_effort', event.target.value || null)
            }
          />
        </Field>
        <Field
          htmlFor="automation-tier"
          label={t('automations.field_service_tier')}
        >
          <Input
            id="automation-tier"
            value={automation.agent.service_tier ?? ''}
            onChange={(event) =>
              updateAgent('service_tier', event.target.value || null)
            }
          />
        </Field>
        <Field
          htmlFor="automation-preset"
          label={t('automations.field_agent_preset')}
        >
          <Input
            id="automation-preset"
            value={automation.agent.agent_preset ?? ''}
            onChange={(event) =>
              updateAgent('agent_preset', event.target.value || null)
            }
          />
        </Field>
        <Field
          htmlFor="automation-permission"
          label={t('automations.field_permission')}
        >
          <Select
            id="automation-permission"
            options={RUNTIME_MODES.map((mode) => ({
              value: mode,
              label: runtimeModeLabel(mode, t),
            }))}
            value={automation.agent.runtime_mode}
            onChange={(value) =>
              updateAgent('runtime_mode', value as RuntimeMode)
            }
          />
        </Field>
        <Field
          htmlFor="automation-interaction"
          label={t('automations.field_interaction')}
        >
          <Select
            id="automation-interaction"
            options={INTERACTION_MODES.map((mode) => ({
              value: mode,
              label: t(`mode.${mode}`),
            }))}
            value={automation.agent.interaction_mode}
            onChange={(value) =>
              updateAgent('interaction_mode', value as InteractionMode)
            }
          />
        </Field>
        <Field
          htmlFor="automation-project"
          label={t('automations.field_project')}
        >
          <Select
            id="automation-project"
            options={[
              { value: 'none', label: t('automations.no_project') },
              ...projects.map((project) => ({
                value: project.id,
                label: project.name,
              })),
            ]}
            value={automation.project_id ?? 'none'}
            onChange={updateProject}
          />
        </Field>
      </div>

      {automation.project_id && (
        <CheckControl
          checked={automation.workspace?.kind === 'newWorktree'}
          label={t('automations.workspace_worktree')}
          onChange={(checked) =>
            setAutomation((current) => ({
              ...current,
              workspace: checked
                ? { kind: 'newWorktree', baseBranch: null }
                : { kind: 'local' },
            }))
          }
        />
      )}

      <Field
        description={t('automations.field_schedule_frequency_description')}
        htmlFor="automation-frequency"
        label={t('automations.field_schedule_frequency')}
      >
        <Select
          id="automation-frequency"
          options={[
            { value: 'manual', label: t('automations.preset_manual') },
            { value: 'hourly', label: t('automations.frequency_hourly') },
            { value: 'daily', label: t('automations.frequency_daily') },
            { value: 'weekly', label: t('automations.frequency_weekly') },
            { value: 'monthly', label: t('automations.frequency_monthly') },
          ]}
          value={schedule.kind}
          onChange={(value) =>
            setAutomation((current) => ({
              ...current,
              schedule: scheduleWithKind(
                current.schedule,
                value as Schedule['kind'],
              ),
            }))
          }
        />
      </Field>

      {schedule.kind !== 'manual' && (
        <div className="grid gap-4 sm:grid-cols-2">
          {schedule.kind === 'hourly' ? (
            <Field
              description={t('automations.field_minute_description')}
              htmlFor="automation-minute"
              label={t('automations.field_time')}
            >
              <NumberInput
                id="automation-minute"
                max={59}
                min={0}
                suffix={t('automations.at_minute')}
                value={time.minute}
                onChange={(value) => updateTime('minute', value)}
              />
            </Field>
          ) : (
            <fieldset className="min-w-0">
              <legend className="text-[12px] font-medium">
                {t('automations.field_time')}
              </legend>
              <p className="mt-1 text-[10.5px] leading-4 text-[var(--text-tertiary)]">
                {t('automations.field_time_description')}
              </p>
              <div className="mt-2 flex items-center gap-2">
                <Input
                  aria-label={t('automations.hour_placeholder')}
                  className="w-20"
                  max={23}
                  min={0}
                  type="number"
                  value={time.hour}
                  onChange={(event) => updateTime('hour', event.target.value)}
                />
                <span aria-hidden="true">:</span>
                <Input
                  aria-label={t('automations.minute_placeholder')}
                  className="w-20"
                  max={59}
                  min={0}
                  type="number"
                  value={time.minute}
                  onChange={(event) => updateTime('minute', event.target.value)}
                />
              </div>
            </fieldset>
          )}
          {schedule.kind === 'weekly' && (
            <fieldset className="min-w-0">
              <legend className="text-[12px] font-medium">
                {t('automations.field_days')}
              </legend>
              <p className="mt-1 text-[10.5px] leading-4 text-[var(--text-tertiary)]">
                {t('automations.field_weekdays_description')}
              </p>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {WEEKDAYS.map((day) => (
                  <CheckControl
                    compact
                    checked={schedule.weekdays.includes(day)}
                    key={day}
                    label={weekdayLabel(day, t)}
                    onChange={() => toggleWeekday(day)}
                  />
                ))}
              </div>
            </fieldset>
          )}
          {schedule.kind === 'monthly' && (
            <Field
              description={t('automations.field_monthdays_description')}
              htmlFor="automation-monthdays"
              label={t('automations.field_days')}
            >
              <Input
                id="automation-monthdays"
                inputMode="numeric"
                value={monthdayText}
                onChange={(event) => updateMonthdays(event.target.value)}
              />
            </Field>
          )}
        </div>
      )}

      <div className="grid gap-4 sm:grid-cols-2">
        <Field
          description={t('automations.field_overlap_description')}
          htmlFor="automation-overlap"
          label={t('automations.field_overlap')}
        >
          <Select
            id="automation-overlap"
            options={OVERLAP_POLICIES.map((policy) => ({
              value: policy,
              label: overlapLabel(policy, t),
            }))}
            value={automation.overlap}
            onChange={(value) =>
              setAutomation((current) => ({
                ...current,
                overlap: value as OverlapPolicy,
              }))
            }
          />
        </Field>
        <Field
          description={t('automations.field_notify_when_description')}
          htmlFor="automation-notification-trigger"
          label={t('automations.field_notify_when')}
        >
          <div className="flex items-center gap-2">
            <CheckControl
              checked={automation.notification.enabled}
              label={t('automations.field_notifications')}
              onChange={(enabled) =>
                setAutomation((current) => ({
                  ...current,
                  notification: { ...current.notification, enabled },
                }))
              }
            />
            <Select
              disabled={!automation.notification.enabled}
              id="automation-notification-trigger"
              options={NOTIFICATION_TRIGGERS.map((trigger) => ({
                value: trigger,
                label: notificationLabel(trigger, t),
              }))}
              value={automation.notification.trigger}
              onChange={(value) =>
                setAutomation((current) => ({
                  ...current,
                  notification: {
                    ...current.notification,
                    trigger: value as NotificationTrigger,
                  },
                }))
              }
            />
          </div>
        </Field>
      </div>
      <CheckControl
        checked={automation.enabled}
        label={t('automations.enabled')}
        onChange={(enabled) =>
          setAutomation((current) => ({ ...current, enabled }))
        }
      />
    </form>
  )
}

function DeleteAutomationDialog({
  automation,
  confirmation,
  pending,
  t,
  onCancel,
  onChange,
  onConfirm,
}: {
  automation: Automation | undefined
  confirmation: AutomationDeleteConfirmation | undefined
  pending: boolean
  t: AutomationTranslator
  onCancel: () => void
  onChange: (cascade: boolean) => void
  onConfirm: () => void
}) {
  return (
    <Dialog
      open={Boolean(automation && confirmation)}
      onOpenChange={(open) => {
        if (!open && !pending) onCancel()
      }}
    >
      <DialogContent className="max-w-[430px]">
        <DialogTitle>
          {t('automations.delete_title', { name: automation?.name ?? '' })}
        </DialogTitle>
        <DialogDescription>
          {t('automations.delete_safe_description')}
        </DialogDescription>
        <label className="mt-4 flex cursor-pointer items-start gap-2 rounded-lg border p-3 text-[12px] outline-none focus-within:ring-3 focus-within:ring-ring/30">
          <input
            checked={confirmation?.cascadeSessions ?? false}
            className="mt-0.5 size-4 accent-primary"
            disabled={pending}
            type="checkbox"
            onChange={(event) => onChange(event.target.checked)}
          />
          <span>
            {t('automations.delete_sessions_exact', {
              count: confirmation?.sessionCount ?? 0,
            })}
          </span>
        </label>
        <p className="mt-3 text-[11px] text-[var(--text-tertiary)]">
          {t('automations.delete_irreversible')}
        </p>
        <div className="mt-5 flex justify-end gap-2">
          <Button
            autoFocus
            disabled={pending}
            variant="ghost"
            onClick={onCancel}
          >
            {t('automations.cancel')}
          </Button>
          <Button disabled={pending} variant="destructive" onClick={onConfirm}>
            <WakuIcon
              className={cn(pending && 'animate-spin')}
              name={pending ? 'loaderCircle' : 'trash'}
            />
            {t('automations.confirm_delete')}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function RunRow({
  locale,
  run,
  selected,
  t,
  onOpen,
}: {
  locale: string
  run: AutomationRun
  selected: boolean
  t: AutomationTranslator
  onOpen: (sessionId: string) => void
}) {
  const sessionId = run.session_id ?? undefined
  return (
    <button
      className={cn(
        'flex min-w-0 items-center gap-2 rounded-lg border px-3 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-default disabled:opacity-80',
        selected
          ? 'border-ring bg-accent'
          : 'border-transparent enabled:hover:bg-accent',
      )}
      disabled={!sessionId}
      type="button"
      onClick={() => sessionId && onOpen(sessionId)}
    >
      <RunStatus outcome={run.outcome} t={t} />
      <span className="min-w-0 flex-1">
        <span className="block text-[11.5px] font-medium">
          {t(`automations.outcome_${run.outcome}`)}
        </span>
        <span className="mt-0.5 block text-[10.5px] text-[var(--text-tertiary)]">
          {automationRunTimeLabel(run.at, locale)}
          {run.catch_up ? ` · ${t('automations.catch_up')}` : ''}
        </span>
      </span>
      {sessionId && (
        <WakuIcon
          className="size-3 text-[var(--text-tertiary)]"
          name="chevronRight"
        />
      )}
    </button>
  )
}

function RunStatus({
  outcome,
  t,
}: {
  outcome: RunOutcome
  t: AutomationTranslator
}) {
  const presentation: Record<
    RunOutcome,
    { icon: WakuIconName; className: string }
  > = {
    running: {
      icon: 'loaderCircle',
      className: 'text-[var(--warning)] motion-safe:animate-spin',
    },
    succeeded: { icon: 'check', className: 'text-[var(--success)]' },
    failed: { icon: 'x', className: 'text-destructive' },
    cancelled: { icon: 'stop', className: 'text-[var(--text-tertiary)]' },
    skipped: { icon: 'queue', className: 'text-[var(--text-tertiary)]' },
  }
  const status = presentation[outcome]
  return (
    <WakuIcon
      className={cn('size-3.5 shrink-0', status.className)}
      label={t(`automations.outcome_${outcome}`)}
      name={status.icon}
    />
  )
}

function EmptyDetails({
  t,
  onNew,
}: {
  t: AutomationTranslator
  onNew: () => void
}) {
  return (
    <div className="flex min-h-64 flex-col items-center justify-center text-center">
      <WakuIcon className="size-7 text-ring" name="zap" />
      <h2 className="mt-3 text-[15px] font-medium">
        {t('automations.empty_title')}
      </h2>
      <p className="mt-1.5 max-w-[300px] text-[12px] leading-5 text-[var(--text-secondary)]">
        {t('automations.select_hint')}
      </p>
      <Button className="mt-4" onClick={onNew}>
        <WakuIcon name="plus" />
        {t('automations.new')}
      </Button>
    </div>
  )
}

function Field({
  children,
  description,
  htmlFor,
  label,
}: {
  children: ReactNode
  description?: string
  htmlFor: string
  label: string
}) {
  return (
    <div className="min-w-0">
      <label className="block text-[12px] font-medium" htmlFor={htmlFor}>
        {label}
      </label>
      {description && (
        <p className="mt-1 text-[10.5px] leading-4 text-[var(--text-tertiary)]">
          {description}
        </p>
      )}
      <div className="mt-2">{children}</div>
    </div>
  )
}

function Select({
  disabled = false,
  id,
  options,
  value,
  onChange,
}: {
  disabled?: boolean
  id: string
  options: Array<{ value: string; label: string }>
  value: string
  onChange: (value: string) => void
}) {
  return (
    <select
      className="h-9 w-full min-w-0 rounded-lg border border-input bg-[var(--inset)] px-2.5 text-[12px] outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 disabled:opacity-50"
      disabled={disabled}
      id={id}
      value={value}
      onChange={(event) => onChange(event.target.value)}
    >
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  )
}

function CheckControl({
  checked,
  compact = false,
  label,
  onChange,
}: {
  checked: boolean
  compact?: boolean
  label: string
  onChange: (checked: boolean) => void
}) {
  return (
    <label
      className={cn(
        'inline-flex cursor-pointer items-center gap-2 rounded-md text-[12px] text-[var(--text-secondary)] outline-none focus-within:ring-3 focus-within:ring-ring/30',
        compact && 'border px-2 py-1 text-[11px]',
      )}
    >
      <input
        checked={checked}
        className="size-4 accent-primary"
        type="checkbox"
        onChange={(event) => onChange(event.target.checked)}
      />
      <span>{label}</span>
    </label>
  )
}

function NumberInput({
  id,
  value,
  min,
  max,
  suffix,
  onChange,
}: {
  id: string
  value: number
  min: number
  max: number
  suffix?: string
  onChange: (value: string) => void
}) {
  return (
    <div className="flex items-center gap-1.5">
      <Input
        className="w-20"
        id={id}
        max={max}
        min={min}
        type="number"
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
      {suffix && (
        <span className="text-[11px] text-[var(--text-tertiary)]">
          {suffix}
        </span>
      )}
    </div>
  )
}

function InfoCard({
  label,
  value,
  icon,
}: {
  label: string
  value: string
  icon: ReactNode
}) {
  return (
    <div className="flex min-w-0 items-center gap-2 rounded-lg border bg-background px-3 py-2.5">
      <span className="text-[var(--text-tertiary)]">{icon}</span>
      <span className="min-w-0">
        <span className="block text-[10px] text-[var(--text-tertiary)]">
          {label}
        </span>
        <span className="mt-0.5 block truncate text-[11.5px]">{value}</span>
      </span>
    </div>
  )
}

function TranscriptState({
  children,
  error = false,
}: {
  children: string
  error?: boolean
}) {
  return (
    <div
      className={cn(
        'rounded-xl border px-3 py-6 text-center text-[12px] text-[var(--text-tertiary)]',
        error && 'text-destructive',
      )}
      role={error ? 'alert' : 'status'}
    >
      {children}
    </div>
  )
}

function providerName(provider: ProviderKind): string {
  return (
    PROVIDERS.find((candidate) => candidate.id === provider)?.name ?? provider
  )
}

function weekdayLabel(day: Weekday, t: AutomationTranslator): string {
  return t(`automations.weekday_${day.slice(0, 3)}`)
}

function overlapLabel(overlap: OverlapPolicy, t: AutomationTranslator): string {
  return t(`automations.overlap_${overlap}`)
}

function notificationLabel(
  trigger: NotificationTrigger,
  t: AutomationTranslator,
): string {
  if (trigger === 'always') return t('automations.notify_always')
  if (trigger === 'onSuccess') return t('automations.notify_success')
  return t('automations.notify_failure')
}

function runtimeModeLabel(mode: RuntimeMode, t: AutomationTranslator): string {
  if (mode === 'autoAcceptEdits') return t('mode.auto_accept_edits')
  if (mode === 'fullAccess') return t('mode.full_access')
  if (mode === 'ask') return t('mode.supervised')
  return t(`mode.${mode}`)
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
