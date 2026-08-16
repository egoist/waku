import type { AgentSession, ProviderKind, ProviderModel, ProviderProbe } from '@waku/client'
import { Popover } from '@base-ui/react/popover'
import { useEffect, useRef, useState, type RefObject } from 'react'
import { ProviderIcon, PROVIDERS, providerMeta, WakuIcon } from '@/components/waku-icon'
import { useDaemonSettings, useProviderProbes } from '@/hooks/use-daemon-data'
import { useI18n } from '@/lib/i18n'
import {
  nextModelPickerHighlight,
  selectedModelPickerIndex,
} from '@/lib/model-picker-presentation'
import { cn } from '@/lib/utils'

type PickerTab = 'favorites' | ProviderKind

export function ModelPicker({
  session,
  currentProbe,
  openSignal,
  onOpenSignalHandled,
  onChange,
  returnFocus,
}: {
  session: AgentSession
  currentProbe?: ProviderProbe
  openSignal?: number
  onOpenSignalHandled?: () => void
  onChange: (provider: ProviderKind, model: ProviderModel) => void
  returnFocus?: RefObject<HTMLElement | null>
}) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [tab, setTab] = useState<PickerTab>(session.provider)
  const [highlight, setHighlight] = useState<number | null>(null)
  const [favorites, setFavorites] = useState<string[]>(() => {
    if (typeof window === 'undefined') return []
    try { return JSON.parse(window.localStorage.getItem('waku.favorite-models') ?? '[]') as string[] }
    catch { return [] }
  })
  const search = useRef<HTMLInputElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const settings = useDaemonSettings()
  const probes = useProviderProbes(open)
  const lockedProvider = session.messages.length ? session.provider : null
  const currentModel = currentProbe?.models.find((model) => model.id === session.model)
    ?? currentProbe?.models.find((model) => model.is_default)
    ?? currentProbe?.models[0]
  const selectedModelId = session.model ?? currentModel?.id
  const selectedName = currentModel?.name ?? session.model ?? providerMeta(session.provider).shortName

  useEffect(() => {
    if (!openSignal) return
    setOpen(true)
    onOpenSignalHandled?.()
  }, [onOpenSignalHandled, openSignal])

  useEffect(() => {
    if (!open) return
    setQuery('')
    setTab(session.provider)
    setHighlight(null)
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
  const selectedIndex = selectedModelPickerIndex(
    rows,
    session.provider,
    selectedModelId,
  )

  useEffect(() => {
    setHighlight((current) => current === null
      ? null
      : Math.min(current, Math.max(0, rows.length - 1)))
  }, [rows.length])

  useEffect(() => {
    if (!open || query.trim()) return
    const frame = requestAnimationFrame(() => {
      if (selectedIndex < 0) {
        list.current?.scrollTo({ top: 0 })
        return
      }
      list.current
        ?.querySelector<HTMLElement>(`[data-model-index="${selectedIndex}"]`)
        ?.scrollIntoView({ block: 'nearest' })
    })
    return () => cancelAnimationFrame(frame)
  }, [open, query, selectedIndex, tab])

  function choose(index: number) {
    const row = rows[index]
    if (!row) return
    onChange(row.provider, row.model)
    setOpen(false)
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
    <Popover.Root modal={false} open={open} onOpenChange={setOpen}>
      <Popover.Trigger
        aria-label={t('models.choose')}
        className={cn(
          'flex h-6 max-w-[224px] items-center gap-1.5 rounded-[6px] px-[7px] text-[11.5px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50',
          open && 'bg-accent text-foreground',
        )}
        disabled={session.status !== 'idle'}
      >
        <ProviderIcon className="size-[10.5px]" provider={session.provider} />
        <span className="truncate">{selectedName}</span>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Positioner
          align="start"
          className="z-[100] outline-none"
          collisionPadding={8}
          side="top"
          sideOffset={4}
        >
          <Popover.Popup
            aria-label={t('models.choose')}
            className="waku-popover-surface flex h-[390px] w-[460px] max-w-[calc(100vw-32px)] overflow-hidden rounded-[12px] outline-none"
            finalFocus={returnFocus
              ? (closeType) => closeType === 'keyboard' ? true : returnFocus.current
              : undefined}
            initialFocus={search}
            role="dialog"
          >
          <div className="flex h-full w-[50px] shrink-0 flex-col items-center gap-1 overflow-y-auto border-r bg-background p-[5px]">
            <ModelTab active={tab === 'favorites' && !query} label={t('models.favorites')} onClick={() => { setTab('favorites'); setQuery(''); setHighlight(null) }}>
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
                  onClick={() => { setTab(provider.id); setQuery(''); setHighlight(null) }}
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
                  aria-activedescendant={highlight !== null && rows[highlight]
                    ? `model-${rows[highlight]!.provider}-${rows[highlight]!.model.id}`
                    : undefined}
                  className="min-w-0 flex-1 bg-transparent text-[12px] outline-none placeholder:text-[var(--text-ghost)]"
                  placeholder={t('input.search_models')}
                  ref={search}
                  value={query}
                  onChange={(event) => {
                    const next = event.target.value
                    setQuery(next)
                    setHighlight(next.trim() ? 0 : null)
                  }}
                  onKeyDown={(event) => {
                    if (event.key === 'ArrowDown') {
                      event.preventDefault()
                      setHighlight((current) => nextModelPickerHighlight(current, rows.length, 'next'))
                    } else if (event.key === 'ArrowUp') {
                      event.preventDefault()
                      setHighlight((current) => nextModelPickerHighlight(current, rows.length, 'previous'))
                    } else if (event.key === 'Enter') {
                      event.preventDefault()
                      choose(highlight ?? (selectedIndex >= 0 ? selectedIndex : 0))
                    } else if (event.key === 'Tab' && !query) {
                      event.preventDefault()
                      const tabs: PickerTab[] = ['favorites', ...usable.map(({ id }) => id)]
                      const current = tabs.indexOf(tab)
                      const delta = event.shiftKey ? -1 : 1
                      setTab(tabs[(current + delta + tabs.length) % tabs.length]!)
                      setHighlight(null)
                    }
                  }}
                />
              </label>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto p-[9px]" ref={list}>
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
                const selected = row.provider === session.provider && row.model.id === selectedModelId
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
                    data-model-index={index}
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
          </Popover.Popup>
        </Popover.Positioner>
      </Popover.Portal>
    </Popover.Root>
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
