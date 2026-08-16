import type { Project } from '@waku/client'

export const PROJECTLESS_NAME = 'No project'

export function isProjectlessProject(project: Pick<Project, 'name'>): boolean {
  return project.name === PROJECTLESS_NAME
}

export function projectDisplayName(
  project: Pick<Project, 'name'>,
  projectlessName: string,
): string {
  return isProjectlessProject(project) ? projectlessName : project.name
}
