export interface ConnectionConfig {
  address: string
  token: string
  remember: boolean
}

const SESSION_KEY = 'waku.remote.session.v1'
const PERSISTENT_KEY = 'waku.remote.persistent.v1'

type ConnectionTranslator = (key: string) => string

export function normalizeDaemonAddress(
  value: string,
  t?: ConnectionTranslator,
): string {
  let address = value.trim()
  if (!address) throw new Error(t?.('web.error_daemon_url_required') ?? 'Enter the daemon URL')

  if (!/^[a-z][a-z\d+.-]*:\/\//i.test(address)) address = `ws://${address}`
  if (/^http:\/\//i.test(address)) address = `ws://${address.slice(7)}`
  if (/^https:\/\//i.test(address)) address = `wss://${address.slice(8)}`

  let url: URL
  try {
    url = new URL(address)
  } catch {
    throw new Error(t?.('web.error_daemon_url_invalid') ?? 'Enter a valid ws:// or wss:// daemon URL')
  }
  if (url.protocol !== 'ws:' && url.protocol !== 'wss:') {
    throw new Error(t?.('web.error_daemon_url_scheme') ?? 'The daemon URL must use ws:// or wss://')
  }
  if (!url.hostname || url.username || url.password) {
    throw new Error(t?.('web.error_daemon_url_host') ?? 'The daemon URL must contain a host and no credentials')
  }
  if (
    typeof window !== 'undefined' &&
    window.location.protocol === 'https:' &&
    url.protocol !== 'wss:'
  ) {
    throw new Error(t?.('web.error_secure_websocket_required') ?? 'A secure Waku Web page can only connect through wss://')
  }

  url.pathname = ''
  url.search = ''
  url.hash = ''
  return url.toString().replace(/\/$/, '')
}

export function validateConnectionConfig(
  value: Omit<ConnectionConfig, 'remember'> & { remember?: boolean },
  t?: ConnectionTranslator,
): ConnectionConfig {
  const token = value.token.trim()
  if (!token) throw new Error(t?.('web.error_daemon_token_required') ?? 'Enter the daemon token')
  return {
    address: normalizeDaemonAddress(value.address, t),
    token,
    remember: value.remember ?? false,
  }
}

export function loadStoredConnection(): ConnectionConfig | null {
  if (typeof window === 'undefined') return null
  return (
    parseStoredConnection(window.sessionStorage.getItem(SESSION_KEY), false) ??
    parseStoredConnection(window.localStorage.getItem(PERSISTENT_KEY), true)
  )
}

export function storeConnection(config: ConnectionConfig): void {
  if (typeof window === 'undefined') return
  clearStoredConnection()
  const storage = config.remember ? window.localStorage : window.sessionStorage
  const key = config.remember ? PERSISTENT_KEY : SESSION_KEY
  storage.setItem(key, JSON.stringify({ address: config.address, token: config.token }))
}

export function clearStoredConnection(): void {
  if (typeof window === 'undefined') return
  window.sessionStorage.removeItem(SESSION_KEY)
  window.localStorage.removeItem(PERSISTENT_KEY)
}

function parseStoredConnection(
  serialized: string | null,
  remember: boolean,
): ConnectionConfig | null {
  if (!serialized) return null
  try {
    const value = JSON.parse(serialized) as Record<string, unknown>
    if (typeof value.address !== 'string' || typeof value.token !== 'string') return null
    return validateConnectionConfig({
      address: value.address,
      token: value.token,
      remember,
    })
  } catch {
    return null
  }
}
