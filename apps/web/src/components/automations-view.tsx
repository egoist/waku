import { useNavigate } from '@tanstack/react-router'
import type { Automation, AutomationRun, RunOutcome } from '@waku/client'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { ConnectionPanel } from '@/components/connection-panel'
import { StartupScreen } from '@/components/startup-screen'
import { Transcript } from '@/components/transcript'
import { WakuIcon, type WakuIconName } from '@/components/waku-icon'
import { useSession, useTaskState } from '@/hooks/use-daemon-data'
import { useDocumentTitle } from '@/hooks/use-document-title'
import {
  automationRunTimeLabel,
  automationScheduleSummary,
  type AutomationTranslator,
} from '@/lib/automations-presentation'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'

/**
 * Read-only web surface for automation schedules, run history, and transcripts.
 *
 * @returns The connected automation catalog and selected run details.
 */
export function AutomationsView() {
  const { locale, t } = useI18n()
  const { phase } = useDaemon()
  const navigate = useNavigate({ from: '/automations' })
  const taskState = useTaskState()
  const [selectedId, setSelectedId] = useState<string | undefined>()
  const [transcriptSessionId, setTranscriptSessionId] = useState<string | undefined>()
  const automations = taskState.data?.automations ?? []
  const selected = automations.find((automation) => automation.id === selectedId)
  const transcript = useSession(transcriptSessionId)

  useDocumentTitle(t('automations.title'))

  useEffect(() => {
    if (!automations.length) {
      setSelectedId(undefined)
      setTranscriptSessionId(undefined)
      return
    }
    if (!selectedId || !automations.some((automation) => automation.id === selectedId)) {
      setSelectedId(automations[0]!.id)
      setTranscriptSessionId(undefined)
    }
  }, [automations, selectedId])

  if (phase !== 'connected') return <ConnectionPanel title={t('automations.title')} />
  if (!taskState.data) {
    return (
      <StartupScreen
        error={taskState.error ? errorMessage(taskState.error) : undefined}
        onRetry={() => void taskState.refetch()}
      />
    )
  }

  const selectedRun = selected?.history?.find(
    (run) => run.session_id === transcriptSessionId,
  )

  return (
    <main className="flex h-dvh min-w-0 flex-col overflow-hidden bg-background">
      <header className="flex min-h-12 shrink-0 items-center gap-3 border-b px-4 py-2 sm:px-6">
        <Button
          aria-label={t('settings.back')}
          size="icon-sm"
          variant="ghost"
          onClick={() => void navigate({ to: '/', search: { session: undefined } })}
        >
          <WakuIcon name="arrowLeft" />
        </Button>
        <div className="min-w-0">
          <h1 className="truncate text-[18px] font-medium">{t('automations.title')}</h1>
          <p className="truncate text-[11.5px] text-[var(--text-tertiary)]">
            {t('automations.web_description')}
          </p>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-4 sm:px-6 sm:py-6">
        <div className="mx-auto flex w-full max-w-[1180px] flex-col gap-4">
          <ClientNotice connected={phase === 'connected'} t={t} />
          <div className="grid min-w-0 grid-cols-1 gap-4 xl:grid-cols-[minmax(280px,360px)_minmax(0,1fr)]">
            <AutomationList
              automations={automations}
              selectedId={selectedId}
              t={t}
              onSelect={(id) => {
                setSelectedId(id)
                setTranscriptSessionId(undefined)
              }}
            />
            <section className="min-w-0 rounded-[13px] border bg-[var(--raised)] p-4 sm:p-5">
              {selected ? (
                <AutomationDetails
                  automation={selected}
                  locale={locale}
                  selectedRun={selectedRun}
                  transcriptSessionId={transcriptSessionId}
                  transcript={transcript.data}
                  transcriptError={transcript.error}
                  transcriptPending={transcript.isPending}
                  t={t}
                  onOpenRun={setTranscriptSessionId}
                  onOpenTask={(sessionId) => void navigate({
                    to: '/',
                    search: { session: sessionId },
                  })}
                />
              ) : (
                <EmptyDetails t={t} />
              )}
            </section>
          </div>
        </div>
      </div>
    </main>
  )
}

function ClientNotice({ connected, t }: { connected: boolean; t: AutomationTranslator }) {
  return (
    <div
      className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-xl border bg-[var(--raised)] px-4 py-3 text-[12px]"
      role="status"
    >
      <WakuIcon
        className={cn('size-3.5', connected ? 'text-[var(--success)]' : 'text-[var(--warning)]')}
        name={connected ? 'check' : 'alert'}
      />
      <span className="font-medium">
        {t(connected ? 'automations.client_connected' : 'automations.client_disconnected')}
      </span>
      <span className="text-[var(--text-secondary)]">{t('automations.daemon_required')}</span>
    </div>
  )
}

function AutomationList({
  automations,
  selectedId,
  t,
  onSelect,
}: {
  automations: Automation[]
  selectedId: string | undefined
  t: AutomationTranslator
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
          <h2 className="mt-3 text-[14px] font-medium">{t('automations.empty_title')}</h2>
          <p className="mt-1.5 max-w-[270px] text-[12px] leading-5 text-[var(--text-secondary)]">
            {t('automations.empty_read_body')}
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-1">
          {automations.map((automation) => (
            <button
              aria-current={selectedId === automation.id ? 'true' : undefined}
              className={cn(
                'w-full rounded-[9px] px-3 py-3 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring',
                selectedId === automation.id ? 'bg-accent' : 'hover:bg-accent/60',
              )}
              key={automation.id}
              type="button"
              onClick={() => onSelect(automation.id)}
            >
              <span className="flex min-w-0 items-center gap-2">
                <WakuIcon
                  className={cn(
                    'size-3.5',
                    automation.enabled ? 'text-[var(--success)]' : 'text-[var(--text-ghost)]',
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
                {automation.enabled ? t('automations.enabled') : t('automations.disabled')}
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
  selectedRun,
  transcriptSessionId,
  transcript,
  transcriptError,
  transcriptPending,
  t,
  onOpenRun,
  onOpenTask,
}: {
  automation: Automation
  locale: string
  selectedRun: AutomationRun | undefined
  transcriptSessionId: string | undefined
  transcript: Parameters<typeof Transcript>[0]['session'] | null | undefined
  transcriptError: Error | null
  transcriptPending: boolean
  t: AutomationTranslator
  onOpenRun: (sessionId: string) => void
  onOpenTask: (sessionId: string) => void
}) {
  const history = automation.history ?? []
  return (
    <div className="flex min-w-0 flex-col gap-5">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="min-w-0 truncate text-[18px] font-medium">{automation.name}</h2>
          <span className={cn(
            'rounded-full px-2 py-0.5 text-[10px] font-medium',
            automation.enabled
              ? 'bg-[color-mix(in_srgb,var(--success)_14%,transparent)] text-[var(--success)]'
              : 'bg-accent text-[var(--text-tertiary)]',
          )}>
            {automation.enabled ? t('automations.enabled') : t('automations.disabled')}
          </span>
        </div>
        <p className="mt-1 text-[12px] text-[var(--text-secondary)]">
          {automationScheduleSummary(automation.schedule, t)}
        </p>
      </div>

      <div className="min-w-0">
        <div className="flex items-center justify-between gap-2">
          <h3 className="text-[12px] font-medium text-[var(--text-secondary)]">
            {t('automations.history')}
          </h3>
          <span className="text-[10.5px] text-[var(--text-tertiary)]">{history.length}</span>
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
              <Button size="sm" variant="ghost" onClick={() => onOpenTask(selectedRun.session_id!)}>
                {t('automations.open_task')}
                <WakuIcon name="arrowRight" />
              </Button>
            )}
          </div>
          {transcriptPending ? (
            <TranscriptState>{t('automations.loading_transcript')}</TranscriptState>
          ) : transcript ? (
            <div className="h-[min(560px,70dvh)] min-h-80 overflow-hidden rounded-xl border bg-background">
              <Transcript session={transcript} />
            </div>
          ) : (
            <TranscriptState error={Boolean(transcriptError)}>
              {transcriptError ? errorMessage(transcriptError) : t('automations.transcript_unavailable')}
            </TranscriptState>
          )}
        </div>
      )}
    </div>
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
        selected ? 'border-ring bg-accent' : 'border-transparent enabled:hover:bg-accent',
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
      {sessionId && <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="chevronRight" />}
    </button>
  )
}

function RunStatus({ outcome, t }: { outcome: RunOutcome; t: AutomationTranslator }) {
  const presentation: Record<RunOutcome, { icon: WakuIconName; className: string }> = {
    running: { icon: 'loaderCircle', className: 'text-[var(--warning)] motion-safe:animate-spin' },
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

function EmptyDetails({ t }: { t: AutomationTranslator }) {
  return (
    <div className="flex min-h-64 flex-col items-center justify-center text-center">
      <WakuIcon className="size-7 text-ring" name="zap" />
      <h2 className="mt-3 text-[15px] font-medium">{t('automations.empty_title')}</h2>
      <p className="mt-1.5 max-w-[300px] text-[12px] leading-5 text-[var(--text-secondary)]">
        {t('automations.select_hint')}
      </p>
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

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
