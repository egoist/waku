export type TranscriptLinkRoute =
  | { kind: 'projectFile'; path: string }
  | { kind: 'remoteFile'; path: string }
  | { kind: 'external' }

export function transcriptLinkRoute(target: string, workspace?: string): TranscriptLinkRoute {
  const path = markdownFilePath(target)
  if (!path) return { kind: 'external' }
  const normalizedPath = normalizePath(path)
  const normalizedWorkspace = workspace ? normalizePath(workspace) : null
  if (normalizedWorkspace) {
    const prefix = normalizedWorkspace === '/' ? '/' : `${normalizedWorkspace}/`
    if (normalizedPath.startsWith(prefix) && normalizedPath !== normalizedWorkspace) {
      return { kind: 'projectFile', path: normalizedPath.slice(prefix.length) }
    }
  }
  return { kind: 'remoteFile', path: normalizedPath }
}

function markdownFilePath(target: string) {
  const stripped = stripFileLocation(target.trim())
  let path: string
  if (stripped.startsWith('/')) path = stripped
  else if (stripped.startsWith('file://localhost/')) path = stripped.slice('file://localhost'.length)
  else if (stripped.startsWith('file:///')) path = stripped.slice('file://'.length)
  else if (stripped.startsWith('file:/')) path = stripped.slice('file:'.length)
  else return null
  try {
    return decodeURIComponent(path)
  } catch {
    return path
  }
}

function stripFileLocation(target: string) {
  const fragment = target.match(/^(.*)#L\d+(?:C\d+)?$/)
  if (fragment) return fragment[1]!
  const lineColumn = target.match(/^(.*):\d+:\d+$/)
  if (lineColumn) return lineColumn[1]!
  const line = target.match(/^(.*):\d+$/)
  return line?.[1] ?? target
}

function normalizePath(path: string) {
  const parts: string[] = []
  for (const part of path.replaceAll('\\', '/').split('/')) {
    if (!part || part === '.') continue
    if (part === '..') parts.pop()
    else parts.push(part)
  }
  return `/${parts.join('/')}`
}
