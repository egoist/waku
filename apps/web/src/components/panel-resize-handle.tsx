import { useRef, type PointerEvent as ReactPointerEvent } from 'react'
import { cn } from '@/lib/utils'

export function PanelResizeHandle({
  edge,
  value,
  min,
  max,
  label,
  className,
  onChange,
  onCommit,
}: {
  edge: 'left' | 'right'
  value: number
  min: number
  max: number
  label: string
  className?: string
  onChange: (value: number) => void
  onCommit?: (value: number) => void
}) {
  const drag = useRef<{ pointerId: number; startX: number; startValue: number } | null>(null)

  function updateFromPointer(event: ReactPointerEvent<HTMLDivElement>) {
    const current = drag.current
    if (!current || current.pointerId !== event.pointerId) return
    const delta = event.clientX - current.startX
    onChange(clamp(current.startValue + (edge === 'right' ? delta : -delta), min, max))
  }

  return (
    <div
      aria-label={label}
      aria-orientation="vertical"
      aria-valuemax={Math.round(max)}
      aria-valuemin={Math.round(min)}
      aria-valuenow={Math.round(value)}
      className={cn(
        'group absolute inset-y-0 z-20 w-2 cursor-col-resize touch-none outline-none',
        edge === 'right' ? '-right-1' : '-left-1',
        className,
      )}
      role="separator"
      tabIndex={0}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 20 : 8
        const direction = edge === 'right' ? 1 : -1
        let next: number | undefined
        if (event.key === 'ArrowLeft') {
          event.preventDefault()
          next = clamp(value - step * direction, min, max)
        } else if (event.key === 'ArrowRight') {
          event.preventDefault()
          next = clamp(value + step * direction, min, max)
        } else if (event.key === 'Home') {
          event.preventDefault()
          next = min
        } else if (event.key === 'End') {
          event.preventDefault()
          next = max
        }
        if (next !== undefined) {
          onChange(next)
          onCommit?.(next)
        }
      }}
      onPointerDown={(event) => {
        if (event.button !== 0) return
        drag.current = {
          pointerId: event.pointerId,
          startX: event.clientX,
          startValue: value,
        }
        event.currentTarget.setPointerCapture(event.pointerId)
        event.preventDefault()
      }}
      onPointerMove={updateFromPointer}
      onPointerUp={(event) => {
        const current = drag.current
        const delta = current ? event.clientX - current.startX : 0
        const next = current
          ? clamp(current.startValue + (edge === 'right' ? delta : -delta), min, max)
          : value
        onChange(next)
        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
          event.currentTarget.releasePointerCapture(event.pointerId)
        }
        drag.current = null
        onCommit?.(next)
      }}
      onPointerCancel={() => {
        drag.current = null
      }}
    >
      <span className="absolute inset-y-0 left-[3px] w-0.5 bg-transparent transition-colors motion-reduce:transition-none group-hover:bg-ring/70 group-focus-visible:bg-ring group-active:bg-ring" />
    </div>
  )
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}
