import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { WakuIcon, type WakuIconName } from '@/components/waku-icon'
import { cn } from '@/lib/utils'

export interface ControlMenuItem {
  id: string
  label: string
  description?: string
  icon?: WakuIconName
  selected?: boolean
  disabled?: boolean
  suffix?: string
  section?: string
  separatorBefore?: boolean
  onSelect: () => void
}

export function ControlMenu({
  label,
  icon,
  children,
  items,
  align = 'left',
  placement = 'above',
  disabled = false,
  caret = true,
  menuClassName,
  triggerClassName,
  onOpenChange,
}: {
  label: string
  icon?: WakuIconName
  children?: ReactNode
  items: ControlMenuItem[]
  align?: 'left' | 'right'
  placement?: 'above' | 'below'
  disabled?: boolean
  caret?: boolean
  menuClassName?: string
  triggerClassName?: string
  onOpenChange?: (open: boolean) => void
}) {
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const root = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const menuId = useId()

  function updateOpen(next: boolean) {
    setOpen(next)
    onOpenChange?.(next)
  }

  function focusItem(index: number) {
    const focus = () => {
      const item = menu.current?.querySelector<HTMLElement>(`[data-menu-index="${index}"]`)
      item?.focus()
      return Boolean(item)
    }
    if (!focus()) requestAnimationFrame(focus)
  }

  function closeToTrigger() {
    trigger.current?.focus()
    updateOpen(false)
  }

  function selectItem(item: ControlMenuItem | undefined) {
    if (!item || item.disabled) return
    trigger.current?.focus()
    updateOpen(false)
    item.onSelect()
  }

  useEffect(() => {
    if (!open) return
    const selected = items.findIndex((item) => item.selected && !item.disabled)
    setActive(selected >= 0 ? selected : Math.max(0, items.findIndex((item) => !item.disabled)))
    const pointer = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) updateOpen(false)
    }
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key === 'Escape' && !event.defaultPrevented) {
        event.preventDefault()
        closeToTrigger()
      }
    }
    document.addEventListener('pointerdown', pointer)
    document.addEventListener('keydown', escape)
    return () => {
      document.removeEventListener('pointerdown', pointer)
      document.removeEventListener('keydown', escape)
    }
  }, [open, items])

  function move(delta: number) {
    if (!items.length) return
    let next = active
    do next = (next + delta + items.length) % items.length
    while (items[next]?.disabled && next !== active)
    setActive(next)
    focusItem(next)
  }

  function onMenuKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      move(1)
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      move(-1)
    } else if (event.key === 'Home') {
      event.preventDefault()
      const next = Math.max(0, items.findIndex((item) => !item.disabled))
      setActive(next)
      focusItem(next)
    } else if (event.key === 'End') {
      event.preventDefault()
      const next = [...items].reverse().findIndex((item) => !item.disabled)
      const index = next < 0 ? 0 : items.length - next - 1
      setActive(index)
      focusItem(index)
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      selectItem(items[active])
    } else if (event.key === 'Escape') {
      event.preventDefault()
      closeToTrigger()
    }
  }

  return (
    <div className="relative shrink-0" ref={root}>
      <button
        aria-controls={open ? menuId : undefined}
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label={label}
        className={cn(
          'flex h-6 max-w-44 items-center gap-1.5 rounded-md px-[7px] text-[11.5px] leading-[14px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-45',
          open && 'bg-accent text-foreground',
          triggerClassName,
        )}
        disabled={disabled}
        ref={trigger}
        type="button"
        onClick={() => updateOpen(!open)}
        onKeyDown={(event) => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault()
            const enabled = items
              .map((item, index) => item.disabled ? -1 : index)
              .filter((index) => index >= 0)
            const next = event.key === 'ArrowDown'
              ? enabled[0] ?? 0
              : enabled[enabled.length - 1] ?? 0
            setActive(next)
            updateOpen(true)
            focusItem(next)
          }
        }}
      >
        {icon && <WakuIcon className="size-[11px] text-[var(--text-tertiary)]" name={icon} />}
        {children ?? <span className="truncate">{label}</span>}
        {caret && <WakuIcon className="size-2.5 text-[var(--text-ghost)]" name="chevronDown" />}
      </button>
      {open && (
        <div
          aria-label={label}
          className={cn(
            'absolute z-[70] min-w-44 overflow-hidden rounded-[10px] border bg-popover p-1 shadow-[0_16px_46px_rgba(0,0,0,0.18)] outline-none',
            placement === 'above' ? 'bottom-[calc(100%+6px)]' : 'top-[calc(100%+6px)]',
            align === 'left' ? 'left-0' : 'right-0',
            menuClassName,
          )}
          id={menuId}
          ref={menu}
          role="menu"
          tabIndex={-1}
          onKeyDown={onMenuKeyDown}
        >
          {items.map((item, index) => (
            <div key={item.id}>
              {item.separatorBefore && index > 0 && (
                <div className="mx-1 my-1 h-px bg-border" role="separator" />
              )}
              {item.section && (index === 0 || items[index - 1]?.section !== item.section) && (
                <div className="px-2 pb-1 pt-2 text-[10px] font-medium text-[var(--text-tertiary)] first:pt-1">
                  {item.section}
                </div>
              )}
            <button
              aria-checked={item.selected}
              className={cn(
                'flex min-h-8 w-full items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[12px] outline-none hover:bg-accent focus-visible:bg-accent disabled:opacity-40',
                active === index && 'bg-accent',
              )}
              disabled={item.disabled}
              data-menu-index={index}
              role="menuitemradio"
              type="button"
              onFocus={() => setActive(index)}
              onMouseEnter={() => setActive(index)}
              onClick={() => {
                selectItem(item)
              }}
            >
              {item.icon && <WakuIcon className="size-3.5 text-[var(--text-tertiary)]" name={item.icon} />}
              <span className="min-w-0 flex-1">
                <span className={cn('flex items-baseline gap-1 truncate font-medium', item.selected && 'font-semibold')}>
                  <span className="truncate">{item.label}</span>
                  {item.suffix && <span className="text-[10.5px] font-normal text-[var(--text-ghost)]">{item.suffix}</span>}
                </span>
                {item.description && (
                  <span className="mt-0.5 block whitespace-normal text-[10.5px] font-normal leading-[14px] text-[var(--text-tertiary)]">
                    {item.description}
                  </span>
                )}
              </span>
              {item.selected && <WakuIcon className="size-[11px] text-[var(--text-tertiary)]" name="check" />}
            </button>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
