import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

function Badge({ className, ...props }: ComponentProps<'span'>) {
  return (
    <span
      data-slot="badge"
      className={cn(
        'inline-flex h-5 items-center gap-1 rounded-md bg-muted px-1.5 text-[11px] font-medium text-muted-foreground',
        className,
      )}
      {...props}
    />
  )
}

export { Badge }
