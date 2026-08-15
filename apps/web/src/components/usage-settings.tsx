import type {
  DaySlice,
  ModelSlice,
  MonthSlice,
  Project,
  ProjectSlice,
  ProviderDay,
  ProviderSlice,
  UsageHistory,
  UsageWindow,
} from '@waku/client'
import { useState, type ReactNode } from 'react'
import { Virtuoso } from 'react-virtuoso'
import { ControlMenu } from '@/components/control-menu'
import { UsageTrendChart, type UsageMetric } from '@/components/usage-chart'
import { ProviderIcon, WakuIcon } from '@/components/waku-icon'
import { useUsageHistory } from '@/hooks/use-daemon-data'
import { useI18n, type AppLocale } from '@/lib/i18n'
import type { Translator } from '@/lib/transcript-presentation'
import { projectDisplayName } from '@/lib/project-presentation'
import { cn } from '@/lib/utils'

type UsageView = 'daily' | 'monthly' | 'projects'
type UsageBreakdown = 'model' | 'day'

const USAGE_WINDOWS: Array<{ window: UsageWindow; labelKey: string }> = [
  { window: { trailingDays: 7 }, labelKey: 'usage.last_7_days' },
  { window: { trailingDays: 30 }, labelKey: 'usage.last_30_days' },
  { window: { trailingDays: 90 }, labelKey: 'usage.last_90_days' },
  { window: 'thisMonth', labelKey: 'usage.this_month' },
  { window: 'lastMonth', labelKey: 'usage.last_month' },
]

export function UsageSettings({ projects }: { projects: Project[] }) {
  const { locale, t } = useI18n()
  const [view, setView] = useState<UsageView>('daily')
  const [window, setWindow] = useState<UsageWindow>({ trailingDays: 30 })
  const [metric, setMetric] = useState<UsageMetric>('cost')
  const [breakdown, setBreakdown] = useState<UsageBreakdown>('model')
  const [projectFilter, setProjectFilter] = useState('')
  const requestedWindow: UsageWindow = view === 'monthly' ? { months: 12 } : window
  const usage = useUsageHistory(requestedWindow, projects)
  const history = usage.data && historyMatchesView(usage.data, view) ? usage.data : undefined
  const range = history
    ? formatUsageRange(history.sinceDay, history.untilDay, view === 'monthly', locale, t)
    : t('usage.scanning')

  return (
    <div className="mt-1.5 flex min-h-0 flex-col">
      <div className="flex min-h-8 flex-wrap items-center gap-2.5">
        <div className="min-w-0 flex-1 truncate text-[12.5px] text-[var(--text-secondary)]">{range}</div>
        <Segmented
          options={[['daily', t('usage.daily')], ['monthly', t('usage.monthly')], ['projects', t('usage.projects')]]}
          value={view}
          onChange={(next) => setView(next as UsageView)}
        />
        {view !== 'monthly' && (
          <ControlMenu
            align="right"
            items={USAGE_WINDOWS.map((option) => ({
              id: usageWindowKey(option.window),
              label: t(option.labelKey),
              selected: usageWindowKey(option.window) === usageWindowKey(window),
              onSelect: () => setWindow(option.window),
            }))}
            label={usageWindowLabel(window, t)}
            menuClassName="w-[170px]"
            placement="below"
            triggerClassName="h-7 w-[124px] max-w-none justify-between border bg-background px-2.5 text-[10.5px]"
          />
        )}
        <button
          aria-label={t(usage.isFetching ? 'usage.scanning' : 'usage.rescan')}
          className="grid size-7 place-items-center rounded-[7px] border text-[var(--text-tertiary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring"
          title={t(usage.isFetching ? 'usage.scanning' : 'usage.rescan')}
          type="button"
          onClick={() => void usage.refetch()}
        >
          <WakuIcon className={cn('size-3', usage.isFetching && 'motion-safe:animate-spin')} name={usage.isFetching ? 'loaderCircle' : 'rotateCw'} />
        </button>
      </div>

      {usage.error && (
        <UsageNotice>{errorMessage(usage.error)}</UsageNotice>
      )}
      {history && (history.errors.length > 0 || history.pricing === 'unavailable') && (
        <UsageNotice>
          {history.errors.map((error) => <div key={error}>{error}</div>)}
          {history.pricing === 'unavailable' && <div>{t('usage.rates_unavailable')}</div>}
        </UsageNotice>
      )}

      {!history ? (
        <UsageSkeleton view={view} />
      ) : (
        <>
          {view === 'daily' && (
            <DailyUsage history={history} metric={metric} breakdown={breakdown} onMetricChange={setMetric} onBreakdownChange={setBreakdown} />
          )}
          {view === 'monthly' && <MonthlyUsage history={history} />}
          {view === 'projects' && (
            <ProjectsUsage history={history} projects={projects} filter={projectFilter} onFilterChange={setProjectFilter} />
          )}
          <div className="mt-4 text-[9.5px] text-[var(--text-ghost)]">
            {t('usage.scan_summary', {
              scanned: formatCount(history.scannedFiles, locale),
              skipped: formatCount(history.skippedFiles, locale),
              records: formatCount(history.records, locale),
              seconds: formatDurationValue(history.scanDuration, locale),
            })}
          </div>
        </>
      )}
    </div>
  )
}

function DailyUsage({
  history,
  metric,
  breakdown,
  onMetricChange,
  onBreakdownChange,
}: {
  history: UsageHistory
  metric: UsageMetric
  breakdown: UsageBreakdown
  onMetricChange: (metric: UsageMetric) => void
  onBreakdownChange: (breakdown: UsageBreakdown) => void
}) {
  const { t } = useI18n()
  return (
    <>
      <div className="mt-5 flex flex-col items-start gap-7 xl:flex-row">
        <UsageSummary history={history} metric={metric} />
        <div className="min-w-0 flex-1 self-stretch">
          <div className="flex flex-wrap items-center gap-3">
            <div className="min-w-0 flex-1 text-[12.5px] font-medium">
              {t(metric === 'cost' ? 'usage.daily_cost' : 'usage.daily_processed_tokens')}
            </div>
            <MiniSegmented
              options={[['cost', t('usage.cost_upper')], ['tokens', t('usage.tokens_upper')]]}
              value={metric}
              onChange={(next) => onMetricChange(next as UsageMetric)}
            />
            <ProviderLegend provider="claude" label="Claude Code" />
            <ProviderLegend provider="codex" label="Codex" />
          </div>
          <div className="mt-2 min-w-0">
            <UsageTrendChart history={history} metric={metric} />
          </div>
        </div>
      </div>

      <UsageMetricStrip history={history} />

      <div className="mt-6 flex flex-col items-start gap-8 xl:flex-row">
        <div className="min-w-0 flex-1 self-stretch">
          <div className="flex items-center gap-3">
            <div className="min-w-0 flex-1 text-[12.5px] font-medium">{t('usage.breakdown')}</div>
            <MiniSegmented
              options={[['model', t('usage.model_upper')], ['day', t('usage.day_upper')]]}
              value={breakdown}
              onChange={(next) => onBreakdownChange(next as UsageBreakdown)}
            />
          </div>
          <div className="mt-2 min-w-0 overflow-x-auto">
            {breakdown === 'model' ? <ModelTable models={history.models} /> : <DayTable days={history.daily} />}
          </div>
        </div>
        <UsageQuality history={history} />
      </div>
    </>
  )
}

function UsageSummary({ history, metric }: { history: UsageHistory; metric: UsageMetric }) {
  const { locale, t } = useI18n()
  const providers = [...history.providers].sort((left, right) => (
    metric === 'cost' ? right.costUsd - left.costUsd : right.totalTokens - left.totalTokens
  ))
  return (
    <div className="w-full shrink-0 xl:w-[300px]">
      <div className="text-[10px] uppercase tracking-[0.02em] text-[var(--text-tertiary)]">
        {t(metric === 'cost' ? 'usage.raw_token_cost' : 'usage.processed_tokens_upper')}
      </div>
      <div className="mt-0.5 text-[30px] font-medium leading-tight tabular-nums">
        {metric === 'cost' ? `${formatMoney(history.costUsd, locale)}*` : formatNumber(history.totalTokens, locale)}
      </div>
      <div className="mt-1 text-[10.5px] text-[var(--text-tertiary)]">
        {metric === 'cost'
          ? t('usage.full_api_rate_note')
          : t(history.sessions === 1 ? 'usage.session_one' : 'usage.session_many', { count: formatCount(history.sessions, locale) })}
      </div>
      <div className="mt-[18px] flex flex-col gap-[18px]">
        {providers.length ? providers.map((provider) => (
          <ProviderSummary key={provider.provider} metric={metric} provider={provider} />
        )) : (
          <div className="text-[11.5px] text-[var(--text-tertiary)]">{t('usage.no_activity_window')}</div>
        )}
      </div>
    </div>
  )
}

function ProviderSummary({ provider, metric }: { provider: ProviderSlice; metric: UsageMetric }) {
  const { locale, t } = useI18n()
  const share = metric === 'cost' ? provider.costShare : provider.tokenShare
  const value = metric === 'cost' ? formatMoney(provider.costUsd, locale) : formatNumber(provider.totalTokens, locale)
  const detail = metric === 'cost'
    ? t('usage.cost_share', {
        share: formatPercent(share, locale),
        tokens: formatNumber(provider.totalTokens, locale),
      })
    : t('usage.token_share', {
        share: formatPercent(share, locale),
        cost: formatMoney(provider.costUsd, locale),
      })
  return (
    <div>
      <div className="flex items-center gap-2">
        <ProviderIcon className={cn('size-3.5', provider.provider === 'claude' ? 'text-[#d97757]' : 'text-foreground')} provider={provider.provider} />
        <div className="min-w-0 flex-1 truncate text-[12.5px]">{provider.provider === 'claude' ? 'Claude Code' : 'Codex'}</div>
        <div className="text-[12.5px] tabular-nums">{value}</div>
      </div>
      <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-[var(--inset)]">
        <div className={cn('h-full rounded-full', provider.provider === 'claude' ? 'bg-[#d97757]' : 'bg-[var(--provider-codex)]')} style={{ width: `${Math.max(0, Math.min(100, share * 100))}%` }} />
      </div>
      <div className="mt-1 text-[10.5px] text-[var(--text-tertiary)]">{detail}</div>
    </div>
  )
}

function UsageMetricStrip({ history }: { history: UsageHistory }) {
  const { locale, t } = useI18n()
  const activeDays = history.daily.filter((day) => day.totalTokens > 0).length
  const dailyAverage = activeDays ? history.totalTokens / activeDays : 0
  const observedInput = history.totals.uncachedInput + history.totals.cachedInput
  const cachedShare = observedInput ? history.totals.cachedInput / observedInput : 0
  const savingsDetail = history.costUsd > 0
    ? t('usage.raw_cost_multiple', {
        multiple: new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(history.quality.cacheSavingsUsd / history.costUsd),
      })
    : t('usage.vs_full_input_rates')
  const tiles = [
    [t('usage.processed_tokens'), formatNumber(history.totalTokens, locale), t('usage.per_active_day', { count: formatNumber(dailyAverage, locale) })],
    [t('usage.cached_input'), formatNumber(history.totals.cachedInput, locale), t('usage.observed_input_share', { share: formatPercent(cachedShare, locale) })],
    [t('usage.uncached_input'), formatNumber(history.totals.uncachedInput, locale), t('usage.cache_writes', { count: formatNumber(history.totals.cacheCreation, locale) })],
    [t('usage.output'), formatNumber(history.totals.output, locale), t('usage.includes_reasoning', { count: formatNumber(history.totals.reasoning, locale) })],
    [t('usage.cache_savings'), formatMoney(history.quality.cacheSavingsUsd, locale), savingsDetail],
  ]
  return (
    <div className="mt-6 grid border-y sm:grid-cols-2 lg:grid-cols-5">
      {tiles.map(([label, value, detail], index) => (
        <div className={cn('min-w-0 px-3.5 py-2.5', index > 0 && 'border-t sm:border-l sm:border-t-0', index === 2 && 'sm:border-l-0 lg:border-l')} key={label}>
          <div className="truncate text-[10px] text-[var(--text-tertiary)]">{label}</div>
          <div className="mt-0.5 truncate text-[15px] tabular-nums">{value}</div>
          <div className="mt-0.5 truncate text-[9.5px] text-[var(--text-tertiary)]">{detail}</div>
        </div>
      ))}
    </div>
  )
}

function ModelTable({ models }: { models: ModelSlice[] }) {
  const { locale, t } = useI18n()
  return (
    <UsageTable
      columns={<><span className="flex-1">{t('usage.model')}</span><span className="w-20 text-right">{t('usage.cost')}</span><span className="w-16 text-right">{t('usage.share')}</span><span className="w-20 text-right">{t('usage.tokens')}</span></>}
    >
      {models.length ? models.map((model) => (
        <div className="flex min-w-[500px] items-center gap-3 border-b py-2 text-[11.5px]" key={`${model.provider}:${model.model}`}>
          <span className="flex min-w-0 flex-1 items-center gap-2">
            <ProviderIcon className={cn('size-3', model.provider === 'claude' ? 'text-[#d97757]' : 'text-foreground')} provider={model.provider} />
            <span className="truncate">{model.model}</span>
          </span>
          <span className="w-20 text-right tabular-nums">{formatMoney(model.costUsd, locale)}</span>
          <span className="w-16 text-right tabular-nums text-[var(--text-tertiary)]">{formatPercent(model.costShare, locale)}</span>
          <span className="w-20 text-right tabular-nums text-[var(--text-tertiary)]">{formatNumber(model.totalTokens, locale)}</span>
        </div>
      )) : <UsageEmpty />}
    </UsageTable>
  )
}

function DayTable({ days }: { days: DaySlice[] }) {
  const { locale, t } = useI18n()
  return (
    <UsageTable
      columns={<><span className="flex-1">{t('usage.day')}</span><span className="w-20 text-right">Claude Code</span><span className="w-20 text-right">Codex</span><span className="w-20 text-right">{t('usage.total')}</span><span className="w-20 text-right">{t('usage.tokens')}</span></>}
    >
      {days.length ? [...days].reverse().slice(0, 8).map((day) => (
        <div className="flex min-w-[600px] items-center gap-3 border-b py-2 text-[11.5px]" key={day.day}>
          <span className="flex-1">{formatDay(day.day, locale)}</span>
          <span className="w-20 text-right tabular-nums text-[var(--text-tertiary)]">{formatMoney(day.byProvider[0].costUsd, locale)}</span>
          <span className="w-20 text-right tabular-nums text-[var(--text-tertiary)]">{formatMoney(day.byProvider[1].costUsd, locale)}</span>
          <span className="w-20 text-right tabular-nums">{formatMoney(day.costUsd, locale)}</span>
          <span className="w-20 text-right tabular-nums text-[var(--text-tertiary)]">{formatNumber(day.totalTokens, locale)}</span>
        </div>
      )) : <UsageEmpty />}
    </UsageTable>
  )
}

function UsageTable({ columns, children }: { columns: ReactNode; children: ReactNode }) {
  return (
    <div>
      <div className="flex min-w-[500px] items-center gap-3 border-b pb-[7px] text-[10.5px] text-[var(--text-tertiary)]">{columns}</div>
      {children}
    </div>
  )
}

function UsageQuality({ history }: { history: UsageHistory }) {
  const { locale, t } = useI18n()
  const rows = [
    [t('usage.provider_reported'), formatPercent(history.quality.providerReportedShare, locale)],
    [t('usage.model_priced'), formatPercent(history.quality.modelPricedShare, locale)],
    [t('usage.unpriced'), formatPercent(history.quality.unpricedShare, locale)],
    [t('usage.cache_savings'), formatMoney(history.quality.cacheSavingsUsd, locale)],
  ]
  return (
    <div className="w-full shrink-0 xl:w-[240px]">
      <div className="text-[12.5px] font-medium">{t('usage.cost_quality')}</div>
      <div className="mt-2">
        {rows.map(([label, value]) => (
          <div className="flex items-center gap-3 border-b py-2 text-[11.5px]" key={label}>
            <span className="min-w-0 flex-1 text-[var(--text-secondary)]">{label}</span>
            <span className="tabular-nums">{value}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

function MonthlyUsage({ history }: { history: UsageHistory }) {
  const { locale, t } = useI18n()
  const byCost = history.costUsd > 0
  const months = [...history.months].reverse()
  const peak = Math.max(0, ...months.map((month) => usageValue(month, byCost)))
  return (
    <UsageListCard
      caption={t('usage.tokens_and_sessions', {
        tokens: formatNumber(history.totalTokens, locale),
        sessions: t(history.sessions === 1 ? 'usage.session_one' : 'usage.session_many', {
          count: formatCount(history.sessions, locale),
        }),
      })}
      title={t('usage.last_12_months')}
      total={byCost ? formatMoney(history.costUsd, locale) : formatNumber(history.totalTokens, locale)}
    >
      <div className="h-[calc(100dvh-210px)] min-h-[360px] overflow-y-auto px-5 pb-2">
        {months.length ? months.map((month) => (
          <MonthUsageRow history={history} key={month.firstDay} locale={locale} month={month} byCost={byCost} peak={peak} t={t} />
        )) : <UsageEmpty message={t('usage.no_activity_12_months')} />}
      </div>
    </UsageListCard>
  )
}

function MonthUsageRow({
  history,
  month,
  byCost,
  peak,
  locale,
  t,
}: {
  history: UsageHistory
  month: MonthSlice
  byCost: boolean
  peak: number
  locale: AppLocale
  t: Translator
}) {
  const currentMonth = history.untilDay.slice(0, 7) === month.firstDay.slice(0, 7)
  return (
    <div className="border-b py-3.5">
      <div className="flex items-baseline gap-2">
        <span className="text-[13px] font-medium">{formatMonth(month.firstDay, locale)}</span>
        {currentMonth && <span className="text-[9.5px] text-[var(--text-ghost)]">{t('usage.so_far')}</span>}
        <span className="flex-1" />
        <span className="text-[14px] font-medium tabular-nums">{byCost ? formatMoney(month.costUsd, locale) : formatNumber(month.totalTokens, locale)}</span>
      </div>
      <div className="mt-0.5 flex items-end gap-3">
        <span className="min-w-0 flex-1 truncate text-[10.5px] text-[var(--text-tertiary)]">
          {t('usage.month_caption', {
            tokens: formatNumber(month.totalTokens, locale),
            sessions: t(month.sessions === 1 ? 'usage.session_one' : 'usage.session_many', { count: formatCount(month.sessions, locale) }),
            days: t(month.activeDays === 1 ? 'usage.active_day_one' : 'usage.active_day_many', { count: formatCount(month.activeDays, locale) }),
          })}
        </span>
        <MonthActivityStrip history={history} month={month} byCost={byCost} />
      </div>
      <UsageSplitBar byProvider={month.byProvider} byCost={byCost} length={peak ? usageValue(month, byCost) / peak : 0} />
      <div className="mt-1.5 flex items-center gap-3 text-[9.5px] text-[var(--text-tertiary)]">
        <ProviderValue provider="claude" value={providerValue(month.byProvider[0], byCost)} byCost={byCost} locale={locale} />
        <ProviderValue provider="codex" value={providerValue(month.byProvider[1], byCost)} byCost={byCost} locale={locale} />
        <span className="min-w-0 flex-1 truncate text-right">{topModelsLabel(month.topModels)}</span>
      </div>
    </div>
  )
}

function ProjectsUsage({
  history,
  projects,
  filter,
  onFilterChange,
}: {
  history: UsageHistory
  projects: Project[]
  filter: string
  onFilterChange: (filter: string) => void
}) {
  const { locale, t } = useI18n()
  const byCost = history.costUsd > 0
  const normalized = filter.trim().toLocaleLowerCase()
  const rows = history.projects.filter((project) => {
    const identity = projectIdentity(project, projects, t)
    return !normalized || `${identity.name} ${identity.path ?? ''}`.toLocaleLowerCase().includes(normalized)
  })
  const peak = Math.max(0, ...rows.map((project) => usageValue(project, byCost)))
  return (
    <UsageListCard
      caption={`${t('usage.projects_shown', {
        shown: formatCount(rows.length, locale),
        projects: formatCount(history.projects.length, locale),
      })} · ${t(history.sessions === 1 ? 'usage.session_one' : 'usage.session_many', {
        count: formatCount(history.sessions, locale),
      })}`}
      title={t('usage.by_project')}
      total={byCost ? formatMoney(history.costUsd, locale) : formatNumber(history.totalTokens, locale)}
      action={(
        <label className="flex h-7 w-[240px] shrink-0 items-center gap-2 rounded-md border bg-background px-2.5 focus-within:border-ring">
          <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="search" />
          <input
            aria-label={t('usage.filter_projects')}
            className="min-w-0 flex-1 bg-transparent text-[11px] outline-none placeholder:text-[var(--text-ghost)]"
            placeholder={t('usage.filter_projects')}
            value={filter}
            onChange={(event) => onFilterChange(event.target.value)}
          />
        </label>
      )}
    >
      <div className="h-[calc(100dvh-210px)] min-h-[360px]">
        {rows.length ? (
          <Virtuoso
            className="h-full pb-2"
            computeItemKey={(_, project) => project.path || 'other-sessions'}
            data={rows}
            itemContent={(_, project) => (
              <div className="px-5">
                <ProjectUsageRow project={project} projects={projects} byCost={byCost} locale={locale} peak={peak} t={t} />
              </div>
            )}
          />
        ) : <UsageEmpty message={t(history.projects.length ? 'usage.no_projects_match' : 'usage.no_activity_window')} />}
      </div>
    </UsageListCard>
  )
}

function ProjectUsageRow({
  project,
  projects,
  byCost,
  peak,
  locale,
  t,
}: {
  project: ProjectSlice
  projects: Project[]
  byCost: boolean
  peak: number
  locale: AppLocale
  t: Translator
}) {
  const identity = projectIdentity(project, projects, t)
  const caption = [
    byCost && project.costShare > 0 ? t('usage.percent_of_cost', { share: formatPercent(project.costShare, locale) }) : null,
    t('usage.token_count', { count: formatNumber(project.totalTokens, locale) }),
    t(project.sessions === 1 ? 'usage.session_one' : 'usage.session_many', { count: formatCount(project.sessions, locale) }),
    project.lastDay ? t('usage.last_active', { date: formatDay(project.lastDay, locale) }) : null,
  ].filter(Boolean).join(' · ')
  return (
    <div className="border-b py-3.5">
      <div className="flex items-baseline gap-2">
        <span className="max-w-[40%] truncate text-[13px] font-medium">{identity.name}</span>
        {identity.path && <span className="min-w-0 flex-1 truncate text-[9.5px] text-[var(--text-ghost)]">{identity.path}</span>}
        <span className="shrink-0 text-[14px] font-medium tabular-nums">{byCost ? formatMoney(project.costUsd, locale) : formatNumber(project.totalTokens, locale)}</span>
      </div>
      <div className="mt-0.5 truncate text-[10.5px] text-[var(--text-tertiary)]">{caption}</div>
      <UsageSplitBar byProvider={project.byProvider} byCost={byCost} length={peak ? usageValue(project, byCost) / peak : 0} />
      <div className="mt-1.5 flex items-center gap-3 text-[9.5px] text-[var(--text-tertiary)]">
        <ProviderValue provider="claude" value={providerValue(project.byProvider[0], byCost)} byCost={byCost} locale={locale} />
        <ProviderValue provider="codex" value={providerValue(project.byProvider[1], byCost)} byCost={byCost} locale={locale} />
        <span className="min-w-0 flex-1 truncate text-right">{topModelsLabel(project.topModels)}</span>
      </div>
    </div>
  )
}

function UsageListCard({ title, caption, total, action, children }: { title: string; caption: string; total: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="mt-5 min-h-0 overflow-hidden rounded-[13px] bg-[var(--raised)]">
      <div className="flex items-center gap-4 border-b px-5 py-3.5">
        <div className="min-w-0 flex-1">
          <div className="text-[13.5px] font-medium">{title}</div>
          <div className="mt-0.5 flex items-baseline gap-1.5">
            <span className="text-[11.5px] font-medium tabular-nums">{total}</span>
            <span className="min-w-0 truncate text-[10.5px] text-[var(--text-tertiary)]">· {caption}</span>
          </div>
        </div>
        {action}
      </div>
      {children}
    </section>
  )
}

function UsageSplitBar({ byProvider, byCost, length }: { byProvider: [ProviderDay, ProviderDay]; byCost: boolean; length: number }) {
  const claude = providerValue(byProvider[0], byCost)
  const codex = providerValue(byProvider[1], byCost)
  const total = claude + codex
  return (
    <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-[var(--inset)]">
      <div className="flex h-full min-w-px overflow-hidden rounded-full" style={{ width: `${Math.max(0, Math.min(100, length * 100))}%` }}>
        <div className="h-full bg-[#d97757]" style={{ width: `${total ? claude / total * 100 : 0}%` }} />
        <div className="h-full bg-[var(--provider-codex)]" style={{ width: `${total ? codex / total * 100 : 0}%` }} />
      </div>
    </div>
  )
}

function MonthActivityStrip({ history, month, byCost }: { history: UsageHistory; month: MonthSlice; byCost: boolean }) {
  const days = history.daily.filter((day) => day.day.startsWith(month.firstDay.slice(0, 7)))
  const peak = Math.max(0, ...days.map((day) => byCost ? day.costUsd : day.totalTokens))
  return (
    <div className="flex h-5 w-[168px] shrink-0 items-end gap-px" aria-hidden="true">
      {days.map((day) => {
        const value = byCost ? day.costUsd : day.totalTokens
        const claude = providerValue(day.byProvider[0], byCost)
        const total = claude + providerValue(day.byProvider[1], byCost)
        return (
          <div className="flex h-full min-w-px flex-1 flex-col-reverse overflow-hidden rounded-t-[1px] bg-[var(--inset)]" key={day.day}>
            <div className="w-full bg-[var(--provider-codex)]" style={{ height: `${peak ? value / peak * 100 * (total ? 1 - claude / total : 0) : 0}%` }} />
            <div className="w-full bg-[#d97757]" style={{ height: `${peak ? value / peak * 100 * (total ? claude / total : 0) : 0}%` }} />
          </div>
        )
      })}
    </div>
  )
}

function ProviderValue({
  provider,
  value,
  byCost,
  locale,
}: {
  provider: 'claude' | 'codex'
  value: number
  byCost: boolean
  locale: AppLocale
}) {
  return (
    <span className="flex items-center gap-1.5">
      <ProviderIcon className={cn('size-[11px]', provider === 'claude' ? 'text-[#d97757]' : 'text-foreground')} provider={provider} />
      {byCost ? formatMoney(value, locale) : formatNumber(value, locale)}
    </span>
  )
}

function ProviderLegend({ provider, label }: { provider: 'claude' | 'codex'; label: string }) {
  return (
    <span className="flex items-center gap-1.5 text-[10.5px] text-[var(--text-secondary)]">
      <ProviderIcon className={cn('size-3', provider === 'claude' ? 'text-[#d97757]' : 'text-foreground')} provider={provider} />
      {label}
    </span>
  )
}

function Segmented({ value, options, onChange }: { value: string; options: Array<[string, string]>; onChange: (value: string) => void }) {
  return (
    <div className="flex overflow-hidden rounded-[7px] border">
      {options.map(([id, label]) => (
        <button
          aria-pressed={value === id}
          className={cn('h-[26px] px-[11px] text-[10.5px] text-[var(--text-secondary)] outline-none hover:text-foreground focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring', value === id && 'bg-accent text-foreground')}
          key={id}
          type="button"
          onClick={() => onChange(id)}
        >
          {label}
        </button>
      ))}
    </div>
  )
}

function MiniSegmented(props: Parameters<typeof Segmented>[0]) {
  return <Segmented {...props} />
}

function UsageNotice({ children }: { children: ReactNode }) {
  return <div className="mt-3 rounded-lg border px-3 py-2 text-[10.5px] leading-4 text-[var(--text-tertiary)]">{children}</div>
}

function UsageSkeleton({ view }: { view: UsageView }) {
  return (
    <div className="mt-5 motion-safe:animate-pulse">
      {view === 'daily' ? (
        <>
          <div className="flex gap-7"><div className="h-52 w-[300px] rounded-xl bg-[var(--raised)]" /><div className="h-52 flex-1 rounded-xl bg-[var(--raised)]" /></div>
          <div className="mt-6 h-20 rounded-xl bg-[var(--raised)]" />
        </>
      ) : <div className="h-[calc(100dvh-210px)] min-h-[360px] rounded-[13px] bg-[var(--raised)]" />}
    </div>
  )
}

function UsageEmpty({ message }: { message?: string }) {
  const { t } = useI18n()
  return <div className="py-8 text-center text-[11.5px] text-[var(--text-tertiary)]">{message ?? t('usage.no_activity_window')}</div>
}

function historyMatchesView(history: UsageHistory, view: UsageView) {
  const monthly = typeof history.window === 'object' && 'months' in history.window
  return view === 'monthly' ? monthly : !monthly
}

function usageWindowKey(window: UsageWindow) {
  return typeof window === 'string' ? window : 'trailingDays' in window ? `days:${window.trailingDays}` : `months:${window.months}`
}

function usageWindowLabel(window: UsageWindow, t: Translator) {
  const option = USAGE_WINDOWS.find((candidate) => usageWindowKey(candidate.window) === usageWindowKey(window))
  return option ? t(option.labelKey) : t('usage.custom')
}

function formatUsageRange(
  start: string,
  end: string,
  monthly: boolean,
  locale: AppLocale,
  t: Translator,
) {
  const formatter = new Intl.DateTimeFormat(locale, monthly
    ? { month: 'short', year: 'numeric', timeZone: 'UTC' }
    : { month: 'short', day: 'numeric', year: start.slice(0, 4) === end.slice(0, 4) ? undefined : 'numeric', timeZone: 'UTC' })
  return t('usage.range', {
    start: formatter.format(parseDay(start)),
    end: formatter.format(parseDay(end)),
  })
}

function formatDay(day: string, locale?: AppLocale) {
  return new Intl.DateTimeFormat(locale, { month: 'short', day: 'numeric', timeZone: 'UTC' }).format(parseDay(day))
}

function formatMonth(day: string, locale?: AppLocale) {
  return new Intl.DateTimeFormat(locale, { month: 'long', year: 'numeric', timeZone: 'UTC' }).format(parseDay(day))
}

function parseDay(day: string) {
  return new Date(`${day}T00:00:00Z`)
}

function projectIdentity(project: ProjectSlice, projects: Project[], t?: Translator) {
  if (!project.path) return { name: t ? t('usage.other_sessions') : 'Other sessions', path: null }
  const known = projects.find((candidate) => candidate.path === project.path)
  const normalized = project.path.replace(/[\\/]+$/, '')
  return {
    name: known
      ? projectDisplayName(known, t ? t('project.no_project_name') : 'No project')
      : normalized.split(/[\\/]/).at(-1) || project.path,
    path: project.path,
  }
}

function topModelsLabel(models: Array<[string, number]>) {
  if (!models.length) return ''
  return models.slice(0, 2).map(([model]) => model).join(' · ')
}

function providerValue(provider: ProviderDay, byCost: boolean) {
  return byCost ? provider.costUsd : provider.totalTokens
}

function usageValue(item: { costUsd: number; totalTokens: number }, byCost: boolean) {
  return byCost ? item.costUsd : item.totalTokens
}

function formatMoney(value: number, locale?: AppLocale) {
  return new Intl.NumberFormat(locale, { style: 'currency', currency: 'USD', maximumFractionDigits: value < 10 ? 2 : 0 }).format(value)
}

function formatNumber(value: number, locale?: AppLocale) {
  return new Intl.NumberFormat(locale, { notation: value >= 10_000 ? 'compact' : 'standard', maximumFractionDigits: 1 }).format(value)
}

function formatPercent(value: number, locale?: AppLocale) {
  return new Intl.NumberFormat(locale, { style: 'percent', maximumFractionDigits: 1 }).format(value)
}

function formatCount(value: number, locale?: AppLocale) {
  return new Intl.NumberFormat(locale).format(value)
}

function formatDurationValue(duration: { secs: number; nanos: number }, locale: AppLocale) {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(
    duration.secs + duration.nanos / 1_000_000_000,
  )
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
