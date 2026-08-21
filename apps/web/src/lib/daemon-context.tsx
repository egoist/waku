import { useQueryClient } from '@tanstack/react-query'
import { WakuClient } from '@waku/client'
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import {
  clearStoredConnection,
  loadStoredConnection,
  storeConnection,
  validateConnectionConfig,
  type ConnectionConfig,
} from './connection'
import { translate, useI18n } from './i18n'

export type ConnectionPhase =
  | 'booting'
  | 'disconnected'
  | 'connecting'
  | 'connected'
  | 'error'

interface DaemonContextValue {
  client: WakuClient | null
  config: ConnectionConfig | null
  phase: ConnectionPhase
  error: string | null
  connect: (config: ConnectionConfig) => Promise<void>
  reconnect: () => Promise<void>
  disconnect: () => void
  forget: () => void
}

const DaemonContext = createContext<DaemonContextValue | null>(null)

export function DaemonProvider({ children }: { children: ReactNode }) {
  const { locale } = useI18n()
  const queryClient = useQueryClient()
  const [client, setClient] = useState<WakuClient | null>(null)
  const [config, setConfig] = useState<ConnectionConfig | null>(null)
  const [phase, setPhase] = useState<ConnectionPhase>('booting')
  const [error, setError] = useState<string | null>(null)
  const generation = useRef(0)
  const bootstrapped = useRef(false)

  const open = useCallback(
    async (candidate: ConnectionConfig) => {
      const normalized = validateConnectionConfig(
        candidate,
        (key) => translate(locale, key),
      )
      const attempt = ++generation.current
      client?.disconnect()
      queryClient.clear()
      setPhase('connecting')
      setError(null)
      setConfig(normalized)

      const next = new WakuClient({
        address: normalized.address,
        token: normalized.token,
      })
      setClient(next)
      try {
        await next.connect()
        if (generation.current !== attempt) {
          next.disconnect()
          return
        }
        storeConnection(normalized)
        setPhase('connected')
      } catch (cause) {
        if (generation.current !== attempt) return
        const message = cause instanceof Error ? cause.message : String(cause)
        setError(message)
        setPhase('error')
        throw cause
      }
    },
    [client, locale, queryClient],
  )

  useEffect(() => {
    if (bootstrapped.current) return
    bootstrapped.current = true
    const stored = loadStoredConnection()
    if (!stored) {
      setPhase('disconnected')
      return
    }
    void open(stored).catch(() => {})
    // Storage is read once at browser startup.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    if (!client || phase !== 'connected') return
    const unsubscribeTaskState = client.subscribeTaskState(() => {
      if (!config) return
      void queryClient.invalidateQueries({
        queryKey: ['daemon', config.address, 'task-state'],
      })
    })
    const timer = window.setInterval(() => {
      if (!client.connected) {
        setError(translate(locale, 'web.daemon_connection_closed'))
        setPhase('error')
      }
    }, 1_000)
    return () => {
      unsubscribeTaskState()
      window.clearInterval(timer)
    }
  }, [client, config, locale, phase, queryClient])

  const reconnect = useCallback(async () => {
    if (!client || !config) throw new Error(translate(locale, 'web.daemon_not_configured'))
    setPhase('connecting')
    setError(null)
    try {
      await client.connect()
      setPhase('connected')
      void queryClient.invalidateQueries()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
      setPhase('error')
      throw cause
    }
  }, [client, config, locale, queryClient])

  const disconnect = useCallback(() => {
    ++generation.current
    client?.disconnect()
    setPhase('disconnected')
    setError(null)
    queryClient.clear()
  }, [client, queryClient])

  const forget = useCallback(() => {
    disconnect()
    clearStoredConnection()
    setClient(null)
    setConfig(null)
  }, [disconnect])

  useEffect(() => () => client?.disconnect(), [client])

  const value: DaemonContextValue = {
    client,
    config,
    phase,
    error,
    connect: open,
    reconnect,
    disconnect,
    forget,
  }

  return <DaemonContext.Provider value={value}>{children}</DaemonContext.Provider>
}

export function useDaemon() {
  const context = useContext(DaemonContext)
  if (!context) throw new Error('useDaemon must be used inside DaemonProvider')
  return context
}
