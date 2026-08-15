import { createFileRoute } from '@tanstack/react-router'
import { WakuShell } from '@/components/waku-shell'

export const Route = createFileRoute('/')({
  validateSearch: (search: Record<string, unknown>) => ({
    session: typeof search.session === 'string' ? search.session : undefined,
  }),
  component: IndexRoute,
})

function IndexRoute() {
  return <WakuShell />
}
