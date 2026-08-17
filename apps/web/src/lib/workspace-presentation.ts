export function shouldShowInitialDestination(
  hasPresentedWorkspace: boolean,
  destination: {
    choosing: boolean
    restoringNewTask: boolean
    hydratingSession: boolean
  },
): boolean {
  if (hasPresentedWorkspace) return false
  return destination.choosing || destination.restoringNewTask || destination.hydratingSession
}
