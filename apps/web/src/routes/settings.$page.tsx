import { Navigate, createFileRoute, useNavigate } from '@tanstack/react-router'
import { ConnectionPanel } from '@/components/connection-panel'
import { StartupScreen } from '@/components/startup-screen'
import {
  SettingsView,
  SETTINGS_PAGES,
  isSettingsPageId,
  type SettingsPageId,
} from '@/components/settings-view'
import { useTaskState } from '@/hooks/use-daemon-data'
import { useDocumentTitle } from '@/hooks/use-document-title'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'

export const Route = createFileRoute('/settings/$page')({
  validateSearch: (search: Record<string, unknown>) => ({
    session: typeof search.session === 'string' ? search.session : undefined,
  }),
  component: SettingsPageRoute,
})

function SettingsPageRoute() {
  const { page } = Route.useParams()
  const search = Route.useSearch()
  if (!isSettingsPageId(page)) {
    return (
      <Navigate
        params={{ page: 'general' }}
        replace
        search={search}
        to="/settings/$page"
      />
    )
  }
  return <SettingsApp page={page} sessionId={search.session} />
}

function SettingsApp({
  page,
  sessionId,
}: {
  page: SettingsPageId
  sessionId?: string
}) {
  const { t } = useI18n()
  const { phase } = useDaemon()
  const pageMetadata = SETTINGS_PAGES.find((candidate) => candidate.id === page)
  const title = pageMetadata ? t(pageMetadata.labelKey) : t('common.settings')
  if (phase !== 'connected') return <ConnectionPanel title={title} />
  return <ConnectedSettings page={page} sessionId={sessionId} />
}

function ConnectedSettings({
  page,
  sessionId,
}: {
  page: SettingsPageId
  sessionId?: string
}) {
  const { t } = useI18n()
  const navigate = useNavigate({ from: '/settings/$page' })
  const taskState = useTaskState()
  const pageMetadata = SETTINGS_PAGES.find((candidate) => candidate.id === page)
  useDocumentTitle(pageMetadata ? t(pageMetadata.labelKey) : t('common.settings'))
  if (!taskState.data) {
    return <StartupScreen error={taskState.error ? errorMessage(taskState.error) : undefined} onRetry={() => void taskState.refetch()} />
  }

  return (
    <SettingsView
      page={page}
      projects={taskState.data.projects}
      onBack={() => void navigate({
        to: '/',
        search: { session: sessionId },
      })}
      onPageChange={(next) => void navigate({
        to: '/settings/$page',
        params: { page: next },
        search: { session: sessionId },
      })}
    />
  )
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
