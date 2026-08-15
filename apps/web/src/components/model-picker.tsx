import type { AgentSession, ProviderKind, ProviderModel, ProviderProbe } from '@waku/client'
import { useEffect, useRef, useState } from 'react'
import { ProviderIcon, PROVIDERS, providerMeta, WakuIcon } from '@/components/waku-icon'
import { useDaemonSettings, useProviderProbes } from '@/hooks/use-daemon-data'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'

type PickerTab = 'favorites' | ProviderKind

export function ModelPicker({
  session,
  currentProbe,
  openSignal,
  onOpenSignalHandled,
  onChange,
}: {
  session: AgentSession
  currentProbe?: ProviderProbe
  openSignal?: number
  onOpenSignalHandled?: () => void
  onChange: (provider: ProviderKind, model: ProviderModel) => void
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [tab, setTab] = useState<PickerTab>(session.provider)
  const [highlight, setHighlight] = useState(0)
  const [favorites, setFavorites] = useState<string[]>(() => {
    if (typeof window === 'undefined') return []
    try { return JSON.parse(window.localStorage.getItem('waku.favorite-models') ?? '[]') as string[] }
    catch { return [] }
  })
  const root = useRef<HTMLDivElement>(null)
  const search = useRef<HTMLInputElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const settings = useDaemonSettings()
  const probes = useProviderProbes(open)
  const lockedProvider = session.messages.length ? session.provider : null
  const currentModel = currentProbe?.models.find((model) => model.id === session.model)
    ?? currentProbe?.models.find((model) => model.is_default)
    ?? currentProbe?.models[0]
  const selectedName = currentModel?.name ?? session.model ?? providerMeta(session.provider).shortName

  function closeAndFocusTrigger() {
    setOpen(false)
    requestAnimationFrame(() => trigger.current?.focus())
  }

  useEffect(() => {
    if (!openSignal) return
    setOpen(true)
    onOpenSignalHandled?.()
  }, [onOpenSignalHandled, openSignal])

  useEffect(() => {
    if (!open) return
    setQuery('')
    setTab(session.provider)
    setHighlight(0)
    requestAnimationFrame(() => search.current?.focus())
    const pointer = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false)
    }
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        closeAndFocusTrigger()
      }
    }
    document.addEventListener('pointerdown', pointer)
    document.addEventListener('keydown', escape)
    return () => {
      document.removeEventListener('pointerdown', pointer)
      document.removeEventListener('keydown', escape)
    }
  }, [open, session.provider])

  const probeMap = ({
    ...(probes.data ?? {}),
    ...(currentProbe ? { [session.provider]: currentProbe } : {}),
  }) as Partial<Record<ProviderKind, ProviderProbe>>

  const usable = PROVIDERS.filter(({ id }) => {
    if (lockedProvider && id !== lockedProvider) return false
    if (id === session.provider) return true
    return !settings.data?.disabled_providers.includes(id) && probeMap[id]?.installed
  })
  const rows = (() => {
    const normalized = query.trim().toLowerCase()
    const providers = normalized ? usable : usable.filter(({ id }) => tab === 'favorites' || id === tab)
    return providers.flatMap(({ id }) => (probeMap[id]?.models ?? [])
      .filter((model) => {
        const key = `${id}:${model.id}`
        if (!normalized && tab === 'favorites' && !favorites.includes(key)) return false
        return !normalized || `${model.name} ${model.id} ${model.sub_provider ?? ''} ${providerMeta(id).name}`.toLowerCase().includes(normalized)
      })
      .map((model) => ({ provider: id, model })))
  })()

  useEffect(() => setHighlight((current) => Math.min(current, Math.max(0, rows.length - 1))), [rows.length])

  function choose(index: number) {
    const row = rows[index]
    if (!row) return
    onChange(row.provider, row.model)
    closeAndFocusTrigger()
  }

  function toggleFavorite(provider: ProviderKind, model: string) {
    const key = `${provider}:${model}`
    setFavorites((current) => {
      const next = current.includes(key) ? current.filter((item) => item !== key) : [...current, key]
      window.localStorage.setItem('waku.favorite-models', JSON.stringify(next))
      return next
    })
  }

  return (
    <div className="relative shrink-0" ref={root}>
      <button
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-label={t('models.choose')}
        className={cn(
          'flex h-6 max-w-[224px] items-center gap-1.5 rounded-md px-[7px] text-[11.5px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50',
          open && 'bg-accent text-foreground',
        )}
        disabled={session.status !== 'idle'}
        ref={trigger}
        type="button"
        onClick={() => setOpen((value) => !value)}
      >
        <ProviderIcon className="size-[10.5px]" provider={session.provider} />
        <span className="truncate">{selectedName}</span>
      </button>
      {open && (
        <div
          aria-label={t('models.choose')}
          className="absolute bottom-[calc(100%+6px)] left-0 z-[75] flex h-[390px] w-[460px] max-w-[calc(100vw-32px)] overflow-hidden rounded-[13px] border bg-[var(--raised)] shadow-[0_18px_50px_rgba(0,0,0,0.22)]"
          role="dialog"
        >
          <div className="flex h-full w-[50px] shrink-0 flex-col items-center gap-1 overflow-y-auto border-r bg-background p-[5px]">
            <ModelTab active={tab === 'favorites' && !query} label={t('models.favorites')} onClick={() => { setTab('favorites'); setQuery(''); setHighlight(0) }}>
              <WakuIcon className="size-[17px]" name="star" />
            </ModelTab>
            <div className="my-[3px] h-px w-[34px] shrink-0 bg-border" />
            {PROVIDERS.map((provider) => {
              const enabled = usable.some((candidate) => candidate.id === provider.id)
              return (
                <ModelTab
                  active={tab === provider.id && !query}
                  disabled={!enabled}
                  key={provider.id}
                  label={provider.name}
                  onClick={() => { setTab(provider.id); setQuery(''); setHighlight(0) }}
                >
                  <ProviderIcon className="size-[18px]" provider={provider.id} />
                </ModelTab>
              )
            })}
          </div>
          <div className="flex min-w-0 flex-1 flex-col bg-card">
            <div className="h-[52px] shrink-0 px-3 pb-2 pt-2.5">
              <label className="flex h-[34px] items-center gap-2 rounded-[9px] bg-[var(--raised)] px-2.5">
                <WakuIcon className="size-[15px] text-[var(--text-secondary)]" name="search" />
                <input
                  aria-activedescendant={rows[highlight] ? `model-${rows[highlight]!.provider}-${rows[highlight]!.model.id}` : undefined}
                  className="min-w-0 flex-1 bg-transparent text-[12px] outline-none placeholder:text-[var(--text-ghost)]"
                  placeholder={t('input.search_models')}
                  ref={search}
                  value={query}
                  onChange={(event) => { setQuery(event.target.value); setHighlight(0) }}
                  onKeyDown={(event) => {
                    if (event.key === 'ArrowDown') {
                      event.preventDefault()
                      setHighlight((current) => rows.length ? (current + 1) % rows.length : 0)
                    } else if (event.key === 'ArrowUp') {
                      event.preventDefault()
                      setHighlight((current) => rows.length ? (current - 1 + rows.length) % rows.length : 0)
                    } else if (event.key === 'Enter') {
                      event.preventDefault()
                      choose(highlight)
                    } else if (event.key === 'Tab' && !query) {
                      event.preventDefault()
                      const tabs: PickerTab[] = ['favorites', ...usable.map(({ id }) => id)]
                      const current = tabs.indexOf(tab)
                      const delta = event.shiftKey ? -1 : 1
                      setTab(tabs[(current + delta + tabs.length) % tabs.length]!)
                      setHighlight(0)
                    }
                  }}
                />
              </label>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto p-[9px]">
              {!rows.length && (
                <div className="grid h-full place-items-center text-[11.5px] text-[var(--text-ghost)]">
                  {t(query
                    ? 'models.none_found'
                    : tab === 'favorites'
                      ? 'models.favorite_hint'
                      : probes.isFetching
                        ? 'models.loading'
                        : 'models.none_reported')}
                </div>
              )}
              {rows.map((row, index) => {
                const selected = row.provider === session.provider && row.model.id === currentModel?.id
                const favorite = favorites.includes(`${row.provider}:${row.model.id}`)
                return (
                  <div
                    aria-selected={selected}
                    className={cn(
                      'flex h-[58px] w-full items-center gap-2.5 rounded-[9px] border border-transparent px-3 text-left outline-none hover:bg-accent',
                      selected && 'bg-accent',
                      index === highlight && 'border-ring bg-accent',
                    )}
                    id={`model-${row.provider}-${row.model.id}`}
                    key={`${row.provider}-${row.model.id}`}
                    role="option"
                    tabIndex={0}
                    onClick={() => choose(index)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault()
                        choose(index)
                      }
                    }}
                    onMouseEnter={() => setHighlight(index)}
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] font-semibold">{row.model.name}</span>
                      <span className="mt-1 flex items-center gap-1.5 truncate text-[11px] text-[var(--text-tertiary)]">
                        <ProviderIcon className="size-[10.5px]" provider={row.provider} />
                        {row.model.sub_provider ?? providerMeta(row.provider).name}
                      </span>
                    </span>
                    <span
                      aria-label={t(favorite ? 'models.remove_favorite' : 'models.add_favorite')}
                      className="grid size-7 shrink-0 place-items-center rounded-md hover:bg-[color:var(--foreground)]/[0.08]"
                      role="button"
                      tabIndex={0}
                      onClick={(event) => { event.stopPropagation(); toggleFavorite(row.provider, row.model.id) }}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault()
                          event.stopPropagation()
                          toggleFavorite(row.provider, row.model.id)
                        }
                      }}
                    >
                      <WakuIcon className={cn('size-3.5 text-[var(--text-ghost)]', favorite && 'text-amber-500')} name={favorite ? 'starFilled' : 'star'} />
                    </span>
                  </div>
                )
              })}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

function ModelTab({ children, label, active, disabled = false, onClick }: { children: React.ReactNode; label: string; active: boolean; disabled?: boolean; onClick: () => void }) {
  return (
    <button
      aria-label={label}
      className={cn('grid size-[38px] shrink-0 place-items-center rounded-[7px] text-[var(--text-tertiary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-35', active && 'bg-accent text-foreground')}
      disabled={disabled}
      type="button"
      onClick={onClick}
    >
      {children}
    </button>
  )
}
