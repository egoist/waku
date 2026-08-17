import { Button } from '@/components/ui/button'
import { WakuIcon } from '@/components/waku-icon'
import { useI18n } from '@/lib/i18n'
import wakuAppIconUrl from '../../../../website/public/app-icon.png'

export function StartupScreen({
  error,
  onRetry,
}: {
  error?: string
  onRetry?: () => void
}) {
  const { t } = useI18n()
  return (
    <main
      aria-busy={!error}
      aria-label={t(error ? 'web.start_failed_label' : 'web.starting_label')}
      className="grid min-h-dvh place-items-center bg-background px-6"
    >
      <div className="flex max-w-sm flex-col items-center text-center">
        <img
          alt=""
          aria-hidden="true"
          className="size-8 rounded-[8px]"
          draggable={false}
          src={wakuAppIconUrl}
        />
        {error ? (
          <>
            <h1 className="mt-4 text-[14px] font-medium">{t('web.load_failed')}</h1>
            <p className="mt-1.5 break-words text-[11.5px] leading-[18px] text-[var(--text-tertiary)]">
              {error}
            </p>
            {onRetry && (
              <Button className="mt-4 rounded-full px-4" size="sm" type="button" onClick={onRetry}>
                <WakuIcon name="rotateCw" />
                {t('web.try_again')}
              </Button>
            )}
          </>
        ) : (
          <span className="mt-3 text-[11px] text-[var(--text-tertiary)]">{t('web.starting')}</span>
        )}
      </div>
    </main>
  )
}
