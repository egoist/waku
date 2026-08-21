import { Menu } from '@base-ui/react/menu'
import { type ReactNode, type RefObject, useState } from 'react'
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
  highlightTriggerWhenOpen = true,
  selectionMode = 'choice',
  onOpenChange,
  returnFocus,
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
  highlightTriggerWhenOpen?: boolean
  selectionMode?: 'choice' | 'status'
  onOpenChange?: (open: boolean) => void
  returnFocus?: RefObject<HTMLElement | null>
}) {
  const [open, setOpen] = useState(false)

  return (
    <Menu.Root
      disabled={disabled}
      highlightItemOnHover={false}
      modal={false}
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        onOpenChange?.(next)
      }}
    >
      <Menu.Trigger
        aria-label={label}
        className={cn(
          'flex h-6 max-w-44 shrink-0 items-center gap-1.5 rounded-[6px] px-[7px] text-[11.5px] leading-[14px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-45',
          highlightTriggerWhenOpen && open && 'bg-accent text-foreground',
          triggerClassName,
        )}
      >
        {icon && <WakuIcon className="size-[11px] text-[var(--text-tertiary)]" name={icon} />}
        {children ?? <span className="truncate">{label}</span>}
        {caret && <WakuIcon className="size-2.5 text-[var(--text-ghost)]" name="chevronDown" />}
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner
          align={align === 'left' ? 'start' : 'end'}
          className="z-[100] outline-none"
          collisionPadding={8}
          side={placement === 'above' ? 'top' : 'bottom'}
          sideOffset={4}
        >
          <Menu.Popup
            aria-label={label}
            className={cn('waku-menu-surface', menuClassName)}
            finalFocus={returnFocus
              ? (closeType) => closeType === 'keyboard' ? true : returnFocus.current
              : undefined}
          >
            {items.map((item, index) => (
              <div key={item.id}>
                {item.separatorBefore && index > 0 && (
                  <div className="waku-menu-separator" role="separator" />
                )}
                {item.section && (index === 0 || items[index - 1]?.section !== item.section) && (
                  <div className="px-2.5 pb-0.5 pt-1.5 text-[10px] font-medium leading-[14px] text-[var(--text-tertiary)]">
                    {item.section}
                  </div>
                )}
                <Menu.Item
                  aria-checked={selectionMode === 'choice' ? item.selected : undefined}
                  aria-current={selectionMode === 'status' && item.selected ? 'true' : undefined}
                  className={cn(
                    'waku-menu-item',
                    item.description && 'py-[5px] text-foreground',
                    item.selected && 'text-foreground',
                  )}
                  disabled={item.disabled}
                  role={selectionMode === 'choice' ? 'menuitemradio' : 'menuitem'}
                  onClick={() => {
                    setOpen(false)
                    item.onSelect()
                  }}
                >
                  {item.icon && <WakuIcon className="size-3 text-current" name={item.icon} />}
                  <span className="min-w-0 flex-1">
                    <span className={cn(
                      'flex items-baseline gap-1 truncate',
                      item.description && 'font-medium',
                      !item.description && item.selected && 'font-medium',
                      item.description && item.selected && 'font-semibold',
                    )}>
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
                </Menu.Item>
              </div>
            ))}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  )
}
