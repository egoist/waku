import { createFileRoute } from '@tanstack/react-router'
import { AutomationsView } from '@/components/automations-view'

export const Route = createFileRoute('/automations')({
  component: AutomationsView,
})
