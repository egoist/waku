import { Dialog as DialogPrimitive } from '@base-ui/react/dialog'
import { WakuIcon } from '@/components/waku-icon'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'

export function PreviewableImage({
  source,
  name,
  buttonClassName,
  imageClassName,
}: {
  source: string
  name: string
  buttonClassName?: string
  imageClassName?: string
}) {
  const { t } = useI18n()
  return (
    <DialogPrimitive.Root>
      <DialogPrimitive.Trigger
        aria-label={t('attachments.open_preview', { name })}
        className={cn(
          'block overflow-hidden outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring',
          buttonClassName,
        )}
        title={t('attachments.open_preview', { name })}
        type="button"
      >
        <img alt={name} className={imageClassName} src={source} />
      </DialogPrimitive.Trigger>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Backdrop className="fixed inset-0 z-[110] bg-black/80 transition-opacity data-[ending-style]:opacity-0 data-[starting-style]:opacity-0 motion-reduce:transition-none" />
        <DialogPrimitive.Viewport className="fixed inset-0 z-[110] grid place-items-center overflow-hidden p-9">
          <DialogPrimitive.Popup
            className="relative flex max-h-full max-w-full min-h-0 min-w-0 flex-col items-center gap-3 outline-none transition-opacity data-[ending-style]:opacity-0 data-[starting-style]:opacity-0 motion-reduce:transition-none"
          >
            <DialogPrimitive.Title className="sr-only">{name}</DialogPrimitive.Title>
            <DialogPrimitive.Close
              aria-label={t('attachments.close_preview')}
              className="absolute -right-[22px] -top-[22px] z-10 grid size-8 place-items-center rounded-full bg-black/50 text-white/90 outline-none hover:bg-black/70 focus-visible:ring-1 focus-visible:ring-white"
              title={t('attachments.close_preview')}
              type="button"
            >
              <WakuIcon className="size-[13px]" name="x" />
            </DialogPrimitive.Close>
            <div className="flex min-h-0 min-w-0 items-center justify-center">
              <img
                alt={name}
                className="block max-h-[calc(100dvh-124px)] max-w-[calc(100dvw-72px)] object-contain"
                src={source}
              />
            </div>
            <div className="max-w-[560px] shrink-0 truncate rounded-full bg-black/50 px-[11px] py-[5px] text-[11.5px] text-white/90">
              {name}
            </div>
          </DialogPrimitive.Popup>
        </DialogPrimitive.Viewport>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}
