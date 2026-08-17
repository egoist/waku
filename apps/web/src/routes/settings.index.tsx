import { Navigate, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/settings/')({
  validateSearch: (search: Record<string, unknown>) => ({
    session: typeof search.session === 'string' ? search.session : undefined,
  }),
  component: SettingsIndexRoute,
})

function SettingsIndexRoute() {
  const search = Route.useSearch()
  return (
    <Navigate
      params={{ page: 'general' }}
      replace
      search={search}
      to="/settings/$page"
    />
  )
}
