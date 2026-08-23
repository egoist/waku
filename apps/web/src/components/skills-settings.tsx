import { useQueryClient } from '@tanstack/react-query'
import type { Project, ProviderKind, SkillEntry, SkillSource } from '@waku/client'
import { useState } from 'react'
import ReactMarkdown from 'react-markdown'
import { Virtuoso } from 'react-virtuoso'
import remarkGfm from 'remark-gfm'
import { toast } from 'sonner'
import { ControlMenu } from '@/components/control-menu'
import { Button } from '@/components/ui/button'
import { ProviderIcon, providerMeta, WakuIcon } from '@/components/waku-icon'
import { useSkills } from '@/hooks/use-daemon-data'
import { useCopyFeedback } from '@/hooks/use-copy-feedback'
import { daemonKeys, setSkillsEnabled, trashSkills } from '@/lib/daemon-api'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n, type AppLocale } from '@/lib/i18n'
import type { Translator } from '@/lib/transcript-presentation'
import { cn } from '@/lib/utils'

type SkillSourceFilter = 'all' | 'shared' | ProviderKind

type SkillListRow =
  | { type: 'section'; key: string; label: string; count: number }
  | { type: 'skill'; key: string; skill: SkillEntry }

export function SkillsSettings({ projects }: { projects: Project[] }) {
  const { locale, t } = useI18n()
  const { client, config } = useDaemon()
  const queryClient = useQueryClient()
  const skills = useSkills(projects)
  const [query, setQuery] = useState('')
  const [source, setSource] = useState<SkillSourceFilter>('all')
  const [selectedKey, setSelectedKey] = useState<number | null>(null)
  const catalog = skills.data?.skills ?? []
  const normalizedQuery = query.trim().toLocaleLowerCase()
  const matches = catalog.filter((skill) => {
    const sourceMatches = source === 'all' || skill.installs.some((install) => skillSourceKey(install.source) === source)
    if (!sourceMatches) return false
    if (!normalizedQuery) return true
    return `${skill.name} ${skill.description} ${skill.project ?? ''} ${skill.installs.map((install) => install.dir).join(' ')}`
      .toLocaleLowerCase()
      .includes(normalizedQuery)
  })
  const selected = matches.find((skill) => skill.rowKey === selectedKey) ?? matches[0]
  const rows = buildSkillRows(matches, t)
  const disabled = catalog.filter((skill) => !skill.enabled).length
  const sourceOptions = availableSkillSources(catalog, t)

  async function mutate(
    action: () => Promise<void>,
    errorKey: string,
    successMessage?: string,
  ) {
    if (!config) return
    try {
      await action()
      await queryClient.invalidateQueries({ queryKey: daemonKeys.skills(config.address) })
      if (successMessage) toast.success(successMessage)
    } catch (error) {
      toast.error(t(errorKey, { error: errorMessage(error) }))
    }
  }

  if (!skills.isPending && catalog.length === 0) {
    return (
      <div className="grid h-full place-items-center px-10 py-10 text-center">
        <div>
          <div className="mx-auto grid size-11 place-items-center rounded-xl bg-[var(--inset)]">
            <WakuIcon className="size-5 text-[var(--text-tertiary)]" name="package" />
          </div>
          <div className="mt-3 text-[13px] font-medium">{t('skills.empty_title')}</div>
          <p className="mx-auto mt-2 max-w-[420px] text-[11.5px] leading-[17px] text-[var(--text-secondary)]">
            {t('skills.empty_description')}
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex h-full min-h-0 w-full bg-background">
      <section className="flex w-[264px] shrink-0 flex-col border-r" aria-label={t('skills.library')}>
        <div className="flex shrink-0 flex-col gap-[7px] px-2.5 pb-2 pt-[22px]">
          <label className="flex h-7 items-center gap-2 rounded-md border bg-[var(--inset)] px-2.5 focus-within:border-ring">
            <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="search" />
            <input
              aria-label={t('skills.search')}
              className="min-w-0 flex-1 bg-transparent text-[11.5px] outline-none placeholder:text-[var(--text-ghost)]"
              placeholder={t('skills.search')}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (!matches.length || !['ArrowDown', 'ArrowUp'].includes(event.key)) return
                event.preventDefault()
                const current = matches.findIndex((skill) => skill.rowKey === selected?.rowKey)
                const delta = event.key === 'ArrowDown' ? 1 : -1
                setSelectedKey(matches[(current + delta + matches.length) % matches.length]!.rowKey)
              }}
            />
          </label>
          <ControlMenu
            icon="package"
            items={sourceOptions.map((option) => ({
              id: option.id,
              label: option.label,
              suffix: String(option.count),
              selected: source === option.id,
              onSelect: () => setSource(option.id),
            }))}
            label={source === 'all' ? t('skills.filter_all') : skillSourceFilterLabel(source, t)}
            menuClassName="w-[220px]"
            placement="below"
            triggerClassName="h-7 w-fit max-w-[230px] border bg-background px-2"
          />
        </div>
        <div className="min-h-0 flex-1">
          {skills.isPending ? (
            <EmptyLine>{t('skills.scanning')}</EmptyLine>
          ) : rows.length ? (
            <Virtuoso
              className="h-full"
              computeItemKey={(_, row) => row.key}
              data={rows}
              itemContent={(_, row) => row.type === 'section' ? (
                <div className="flex items-baseline gap-1.5 px-[17px] pb-1 pt-3 text-[9.5px] font-semibold uppercase text-[var(--text-tertiary)]">
                  <span className="truncate">{row.label}</span>
                  <span className="font-normal text-[var(--text-ghost)]">{row.count}</span>
                </div>
              ) : (
                <div className="px-2 pb-px">
                  <button
                    aria-current={selected?.rowKey === row.skill.rowKey ? 'true' : undefined}
                    className={cn(
                      'flex min-h-[48px] w-full items-center gap-[9px] rounded-lg px-[9px] py-[7px] text-left outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring',
                      selected?.rowKey === row.skill.rowKey && 'bg-sidebar-accent',
                    )}
                    type="button"
                    onClick={() => setSelectedKey(row.skill.rowKey)}
                  >
                    <span className="grid size-[26px] shrink-0 place-items-center rounded-md bg-[var(--inset)]">
                      <SkillGlyph enabled={row.skill.enabled} skill={row.skill} />
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-1.5">
                        <span className={cn('min-w-0 flex-1 truncate text-[12px] font-medium', !row.skill.enabled && 'text-[var(--text-secondary)]')}>
                          {row.skill.name}
                        </span>
                        {!row.skill.enabled && <span className="shrink-0 text-[8.5px] text-[var(--warning)]">{t('skills.disabled_badge')}</span>}
                      </span>
                      <span className="mt-px block truncate text-[10px] text-[var(--text-tertiary)]">
                        {row.skill.description || skillSourcesLabel(row.skill, t)}
                      </span>
                    </span>
                  </button>
                </div>
              )}
            />
          ) : (
            <EmptyLine>{t('skills.no_match')}</EmptyLine>
          )}
        </div>
        <div className="flex h-[26px] shrink-0 items-center justify-center border-t px-3 text-[9.5px] text-[var(--text-ghost)]">
          {normalizedQuery || source !== 'all'
            ? t('skills.filter_caption', { shown: matches.length, total: catalog.length })
            : `${t(catalog.length === 1 ? 'skills.count_one' : 'skills.count_many', { count: catalog.length })}${disabled ? ` · ${t('skills.count_disabled', { count: disabled })}` : ''}`}
        </div>
      </section>
      <section className="min-w-0 flex-1" aria-label={t('skills.details')}>
        {selected ? (
          <SkillDetail
            key={selected.rowKey}
            locale={locale}
            skill={selected}
            onDelete={() => client
              ? mutate(
                  () => trashSkills(client, selected.installs.map((install) => install.dir)),
                  'skills.delete_failed',
                  t('skills.deleted_toast', { name: selected.name }),
                )
              : Promise.resolve()}
            onToggle={(enabled) => client
              ? mutate(
                  () => setSkillsEnabled(client, selected.installs.map((install) => install.dir), enabled),
                  'skills.toggle_failed',
                )
              : Promise.resolve()}
          />
        ) : (
          <div className="grid h-full place-items-center text-[11px] text-[var(--text-ghost)]">
            <div className="flex flex-col items-center gap-2">
              <WakuIcon className="size-5" name="package" />
              {t('skills.select_placeholder')}
            </div>
          </div>
        )}
      </section>
    </div>
  )
}

function SkillDetail({
  skill,
  locale,
  onToggle,
  onDelete,
}: {
  skill: SkillEntry
  locale: AppLocale
  onToggle: (enabled: boolean) => Promise<void>
  onDelete: () => Promise<void>
}) {
  const { t } = useI18n()
  const copyFeedback = useCopyFeedback()
  const [deleteArmed, setDeleteArmed] = useState(false)
  const location = skill.installs[0]?.dir ?? ''
  const scope = skill.project
    ? t('skills.scope_in_project', { project: skill.project })
    : t('skills.scope_user_detail')
  const supporting = skill.supportingFiles
    ? `${t(skill.supportingFiles === 1 ? 'skills.file_count_one' : 'skills.file_count_many', { count: skill.supportingFiles })} · `
    : ''

  return (
    <div className="h-full overflow-y-auto px-6 pb-5 pt-[18px]">
      <div className="mx-auto max-w-[680px]">
        <div className="flex items-center gap-3">
          <div className="grid size-[38px] shrink-0 place-items-center rounded-[9px] bg-[var(--inset)]">
            <SkillGlyph enabled={skill.enabled} large skill={skill} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h1 className={cn('truncate text-[15px] font-medium', !skill.enabled && 'text-[var(--text-secondary)]')}>{skill.name}</h1>
              {!skill.enabled && <span className="text-[9.5px] text-[var(--warning)]">{t('skills.disabled_badge')}</span>}
            </div>
            <div className="mt-0.5 truncate text-[10.5px] text-[var(--text-tertiary)]">{skillSourcesLabel(skill, t)} · {scope}</div>
          </div>
          <Toggle
            checked={skill.enabled}
            label={t(skill.enabled ? 'skills.disable_named' : 'skills.enable_named', { name: skill.name })}
            onChange={(enabled) => void onToggle(enabled)}
          />
        </div>

        <p className="mt-3.5 text-[11.5px] leading-[17px] text-[var(--text-secondary)]">{skill.description || t('skills.no_description')}</p>

        <dl className="mt-4 text-[10.5px]">
          <SkillInfoRow label={t('skills.detail_invoke')}><span className="font-mono text-[var(--text-secondary)]">/{skill.name}</span></SkillInfoRow>
          {skill.installs.map((install) => (
            <SkillInfoRow key={install.dir} label={skill.installs.length > 1 ? skillSourceLabel(install.source, t) : t('skills.detail_location')}>
              <span className="truncate font-mono text-[10px] text-[var(--text-secondary)]" title={install.dir}>{install.dir}</span>
            </SkillInfoRow>
          ))}
          <SkillInfoRow label={t('skills.detail_contents')}><span className="text-[var(--text-secondary)]">{supporting}{formatBytes(skill.totalBytes, locale)}</span></SkillInfoRow>
          {skill.modifiedAt && <SkillInfoRow label={t('skills.detail_updated')}><span className="text-[var(--text-secondary)]">{formatRelativeTime(skill.modifiedAt, locale, t)}</span></SkillInfoRow>}
          {skill.allowedTools && <SkillInfoRow label={t('skills.allowed_tools')}><span className="truncate font-mono text-[10px] text-[var(--text-secondary)]">{skill.allowedTools}</span></SkillInfoRow>}
        </dl>

        {skill.duplicates > 0 && (
          <div className="mt-3 flex items-center gap-1.5 text-[10px] text-[var(--warning)]">
            <WakuIcon className="size-[11px]" name="alert" />
            {t(skill.duplicates === 1 ? 'skills.duplicate_one' : 'skills.duplicate_many', { count: skill.duplicates })}
          </div>
        )}

        <div className="mt-4 flex flex-wrap items-center gap-[7px]">
          <Button
            className="h-[26px] rounded-md px-2.5 text-[10.5px] font-normal text-[var(--text-secondary)]"
            size="sm"
            variant="outline"
            onClick={() => void copyFeedback.copyText(location)}
          >
            <WakuIcon className="size-[11px]" name={copyFeedback.copied ? 'check' : 'copy'} />
            {t(copyFeedback.copied ? 'common.copied' : 'skills.copy_path')}
          </Button>
          <span className="flex-1" />
          <Button
            className="h-[26px] rounded-md px-2.5 text-[10.5px] font-normal"
            size="sm"
            variant={deleteArmed ? 'destructive' : 'outline'}
            onBlur={() => setDeleteArmed(false)}
            onClick={() => {
              if (deleteArmed) void onDelete()
              else setDeleteArmed(true)
            }}
          >
            <WakuIcon className="size-[11px]" name="trash" /> {t(deleteArmed ? 'skills.confirm_delete' : 'skills.delete')}
          </Button>
        </div>

        {skill.body && (
          <div className="mt-[18px] border-t pt-3.5">
            <div className="font-mono text-[9.5px] text-[var(--text-ghost)]">SKILL.md</div>
            <div className="skill-markdown mt-2.5 text-[11.5px] leading-[18px] text-[var(--text-secondary)]">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{skill.body}</ReactMarkdown>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

function SkillInfoRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline gap-3 border-b py-2 last:border-b-0">
      <dt className="w-[84px] shrink-0 text-[var(--text-tertiary)]">{label}</dt>
      <dd className="flex min-w-0 flex-1">{children}</dd>
    </div>
  )
}

function SkillGlyph({ skill, enabled, large = false }: { skill: SkillEntry; enabled: boolean; large?: boolean }) {
  const source = skill.installs.length === 1 ? skill.installs[0]?.source : 'shared'
  const className = cn(large ? 'size-[18px]' : 'size-[13px]', !enabled && 'opacity-45')
  if (source && source !== 'shared') return <ProviderIcon className={className} provider={source.provider} />
  return <WakuIcon className={cn(className, 'text-[var(--text-secondary)]')} name="package" />
}

function Toggle({ checked, label, onChange }: { checked: boolean; label: string; onChange: (checked: boolean) => void }) {
  return (
    <button
      aria-checked={checked}
      aria-label={label}
      className={cn(
        'flex h-5 w-9 shrink-0 items-center rounded-full border p-0.5 outline-none transition-colors focus-visible:ring-1 focus-visible:ring-ring',
        checked ? 'justify-end border-foreground bg-foreground' : 'justify-start border-input bg-[var(--inset)]',
      )}
      role="switch"
      type="button"
      onClick={() => onChange(!checked)}
    >
      <span className={cn('size-3.5 rounded-full', checked ? 'bg-background' : 'bg-[var(--text-tertiary)]')} />
    </button>
  )
}

function EmptyLine({ children }: { children: React.ReactNode }) {
  return <div className="py-10 text-center text-[11.5px] text-[var(--text-tertiary)]">{children}</div>
}

function buildSkillRows(skills: SkillEntry[], t: Translator) {
  const groups = new Map<string, SkillEntry[]>()
  for (const skill of skills) {
    const key = skill.project ?? ''
    const group = groups.get(key)
    if (group) group.push(skill)
    else groups.set(key, [skill])
  }
  const rows: SkillListRow[] = []
  for (const [project, groupedSkills] of groups) {
    rows.push({
      type: 'section',
      key: `section:${project || 'user'}`,
      label: project || t('skills.section_user_skills'),
      count: groupedSkills.length,
    })
    for (const skill of groupedSkills) rows.push({ type: 'skill', key: `skill:${skill.rowKey}`, skill })
  }
  return rows
}

function availableSkillSources(skills: SkillEntry[], t: Translator) {
  const counts = new Map<Exclude<SkillSourceFilter, 'all'>, number>()
  for (const skill of skills) {
    const seen = new Set<Exclude<SkillSourceFilter, 'all'>>()
    for (const install of skill.installs) seen.add(skillSourceKey(install.source))
    for (const key of seen) counts.set(key, (counts.get(key) ?? 0) + 1)
  }
  const ids: Array<Exclude<SkillSourceFilter, 'all'>> = ['shared', 'claude', 'codex', 'cursor', 'fx', 'openCode', 'pi', 'ohMyPi', 'amp', 'deepSeek', 'grok']
  return [
    { id: 'all' as const, label: t('skills.filter_all'), count: skills.length },
    ...ids.filter((id) => counts.has(id)).map((id) => ({ id, label: skillSourceFilterLabel(id, t), count: counts.get(id)! })),
  ]
}

function skillSourceKey(source: SkillSource): Exclude<SkillSourceFilter, 'all'> {
  return source === 'shared' ? 'shared' : source.provider
}

function skillSourceLabel(source: SkillSource, t: Translator) {
  return source === 'shared' ? t('skills.source_shared') : providerMeta(source.provider).shortName
}

function skillSourceFilterLabel(source: Exclude<SkillSourceFilter, 'all'>, t: Translator) {
  return source === 'shared' ? t('skills.source_shared') : providerMeta(source).shortName
}

function skillSourcesLabel(skill: SkillEntry, t: Translator) {
  return [...new Set(skill.installs.map((install) => skillSourceLabel(install.source, t)))].join(' · ')
}

function formatBytes(value: number, locale: AppLocale) {
  const format = (amount: number) => new Intl.NumberFormat(locale, {
    maximumFractionDigits: 1,
  }).format(amount)
  if (value < 1_024) return `${format(value)} B`
  if (value < 1_048_576) return `${format(value / 1_024)} KB`
  return `${format(value / 1_048_576)} MB`
}

function formatRelativeTime(timestamp: number, locale: AppLocale, t: Translator) {
  const seconds = Math.max(0, Math.floor(Date.now() / 1_000) - timestamp)
  if (seconds < 60) return t('skills.updated_just_now')
  if (seconds < 3_600) return t('skills.updated_minutes', { count: Math.floor(seconds / 60) })
  if (seconds < 86_400) return t('skills.updated_hours', { count: Math.floor(seconds / 3_600) })
  if (seconds < 2_592_000) return t('skills.updated_days', { count: Math.floor(seconds / 86_400) })
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium' }).format(new Date(timestamp * 1_000))
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
