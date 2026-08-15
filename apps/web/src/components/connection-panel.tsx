import { useEffect, useState, type FormEvent } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { StartupScreen } from '@/components/startup-screen'
import { WakuIcon } from '@/components/waku-icon'
import { useDocumentTitle } from '@/hooks/use-document-title'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import wakuAppIconUrl from '../../../../website/public/app-icon.png'

export function ConnectionPanel({ title }: { title?: string } = {}) {
  const { t } = useI18n()
  const { config, phase, error, connect } = useDaemon()
  useDocumentTitle(title)
  const [address, setAddress] = useState(config?.address ?? '')
  const [token, setToken] = useState(config?.token ?? '')
  const [tokenRevealed, setTokenRevealed] = useState(false)
  const [remember, setRemember] = useState(config?.remember ?? false)
  const [localError, setLocalError] = useState<string | null>(null)

  useEffect(() => {
    if (!config) return
    setAddress(config.address)
    setToken(config.token)
    setTokenRevealed(false)
    setRemember(config.remember)
  }, [config])

  if (phase === 'booting' || (phase === 'connecting' && config)) {
    return <StartupScreen />
  }

  async function submit(event: FormEvent) {
    event.preventDefault()
    setLocalError(null)
    try {
      await connect({ address, token, remember })
    } catch (cause) {
      setLocalError(cause instanceof Error ? cause.message : String(cause))
    }
  }

  return (
    <main className="flex min-h-dvh w-full items-center justify-center overflow-y-auto bg-background p-5 pb-12">
      <div className="w-full max-w-[520px]">
        <div className="text-center">
          <img
            alt="Waku"
            className="mx-auto size-8 rounded-[8px]"
            draggable={false}
            src={wakuAppIconUrl}
          />
          <h1 className="mt-3 text-xl font-medium tracking-tight">{t('web.connect_title')}</h1>
          <p className="mx-auto mt-2 max-w-sm text-[12.5px] leading-[19px] text-[var(--text-tertiary)]">
            {t('web.connect_description')}
          </p>
        </div>

        <section className="mt-6 rounded-[13px] bg-[var(--raised)] p-5">
          <form className="space-y-4" onSubmit={submit}>
            <label className="grid gap-1.5 sm:grid-cols-[120px_1fr] sm:items-center">
              <span className="text-[12px] font-medium">{t('daemon.websocket_url')}</span>
              <Input
                autoCapitalize="none"
                autoCorrect="off"
                className="bg-card"
                inputMode="url"
                placeholder="wss://waku.example.com"
                value={address}
                onChange={(event) => setAddress(event.target.value)}
              />
            </label>
            <div className="grid gap-1.5 sm:grid-cols-[120px_1fr] sm:items-center">
              <label className="text-[12px] font-medium" htmlFor="daemon-connection-token">
                {t('daemon.token')}
              </label>
              <div className="relative">
                <Input
                  autoComplete="current-password"
                  className="bg-card pr-10"
                  id="daemon-connection-token"
                  placeholder={t('web.token_placeholder')}
                  type={tokenRevealed ? 'text' : 'password'}
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                />
                <Button
                  aria-label={t(tokenRevealed ? 'daemon.hide_token' : 'daemon.reveal_token')}
                  aria-pressed={tokenRevealed}
                  className="absolute right-1 top-1 size-7 text-[var(--text-tertiary)]"
                  size="icon-sm"
                  title={t(tokenRevealed ? 'daemon.hide_token' : 'daemon.reveal_token')}
                  type="button"
                  variant="ghost"
                  onClick={() => setTokenRevealed((revealed) => !revealed)}
                >
                  <WakuIcon name={tokenRevealed ? 'eyeOff' : 'eye'} />
                </Button>
              </div>
            </div>
            <label className="flex cursor-pointer items-start gap-2.5 border-t pt-4 text-[12px]">
              <input
                checked={remember}
                className="mt-0.5 size-4 accent-foreground"
                type="checkbox"
                onChange={(event) => setRemember(event.target.checked)}
              />
              <span>
                {t('web.remember_device')}
                <span className="mt-0.5 block text-[10.5px] leading-4 text-[var(--text-tertiary)]">
                  {t('web.remember_device_description')}
                </span>
              </span>
            </label>
            {(localError || error) && (
              <div role="alert" className="rounded-lg bg-[var(--danger-soft)] px-3 py-2 text-[12px] text-destructive">
                {localError || error}
              </div>
            )}
            <div className="flex justify-end">
              <Button className="rounded-full px-4" type="submit" disabled={phase === 'connecting'}>
                {phase === 'connecting' ? t('web.connecting') : t('web.connect')}
                {phase !== 'connecting' && <WakuIcon name="arrowRight" />}
              </Button>
            </div>
          </form>
        </section>

        <div className="mt-3 flex gap-2 rounded-lg bg-accent px-3 py-2 text-[10.5px] leading-4 text-[var(--text-tertiary)]">
          <WakuIcon className="mt-0.5 size-3.5 shrink-0" name="lock" />
          <p>
            {t('web.security_warning')}
          </p>
        </div>
      </div>
    </main>
  )
}
