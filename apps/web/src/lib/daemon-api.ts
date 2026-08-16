import type {
  AgentSession,
  AgentInvocation,
  Automation,
  AutomationChange,
  BranchSnapshot,
  Checkpoint,
  CommitSnapshot,
  ComposerDraftChange,
  ComposerDrafts,
  DaemonSettings,
  FileEntry,
  MessageAttachment,
  PlanUsage,
  Project,
  ProviderKind,
  ProviderProbe,
  ReviewDiffData,
  ReviewDiffSource,
  ResponsePayload,
  SessionMessageMatch,
  SlashCommand,
  SkillsCatalog,
  UsageHistory,
  UsageWindow,
  WakuClient,
  WorkingTreeEntry,
  WorkspaceOperation,
  WorkspaceResult,
} from '@waku/client'

export type TaskState = Extract<ResponsePayload, { type: 'taskState' }>
export type DaemonDirectory = Extract<WorkspaceResult, { type: 'directory' }>

export const daemonKeys = {
  taskState: (address: string) => ['daemon', address, 'task-state'] as const,
  composerDrafts: (address: string) => ['daemon', address, 'composer-drafts'] as const,
  session: (address: string, sessionId: string) =>
    ['daemon', address, 'session', sessionId] as const,
  settings: (address: string) => ['daemon', address, 'settings'] as const,
  providers: (address: string) => ['daemon', address, 'providers'] as const,
  provider: (address: string, provider: ProviderKind, binaryOverride: string | null = null) =>
    [...daemonKeys.providers(address), 'catalog', provider, binaryOverride] as const,
  providerDetection: (address: string, provider: ProviderKind) =>
    [...daemonKeys.providers(address), 'detection', provider] as const,
  planUsage: (address: string, provider: ProviderKind) =>
    ['daemon', address, 'plan-usage', provider] as const,
  skills: (address: string) => ['daemon', address, 'skills'] as const,
  usage: (address: string, window: UsageWindow) =>
    ['daemon', address, 'usage', JSON.stringify(window)] as const,
  workspace: (address: string, cwd: string) =>
    ['daemon', address, 'workspace', cwd] as const,
  sessionTurnRefsRoot: (address: string) =>
    ['daemon', address, 'session-turn-refs'] as const,
  sessionTurnRefs: (address: string, cwd: string, sessionId: string) =>
    [...daemonKeys.sessionTurnRefsRoot(address), cwd, sessionId] as const,
  composerSources: (address: string) => ['daemon', address, 'composer-sources'] as const,
  composerFiles: (address: string, cwd: string) =>
    [...daemonKeys.composerSources(address), 'files', cwd] as const,
  slashCommands: (address: string, provider: ProviderKind, cwd: string) =>
    [...daemonKeys.composerSources(address), 'commands', provider, cwd] as const,
  workspaceTree: (address: string, cwd: string, expanded: string[]) =>
    ['daemon', address, 'workspace-tree', cwd, ...[...expanded].sort()] as const,
  directory: (address: string, path: string | null) =>
    ['daemon', address, 'directory', path] as const,
  workspaceFile: (address: string, cwd: string, path: string) =>
    ['daemon', address, 'workspace-file', cwd, path] as const,
  workspaceDiff: (address: string, cwd: string, source: ReviewDiffSource = 'uncommitted') =>
    ['daemon', address, 'workspace-diff', cwd, JSON.stringify(source)] as const,
}

export async function loadTaskState(client: WakuClient): Promise<TaskState> {
  return expectResponse(await client.request({ type: 'loadTaskState' }), 'taskState')
}

/**
 * Starts one automation through the daemon-owned scheduler execution path.
 *
 * @param client Connected daemon client.
 * @param automationId Automation to run immediately.
 * @returns The updated automation and spawned session.
 */
export async function runAutomation(
  client: WakuClient,
  automationId: string,
): Promise<Extract<ResponsePayload, { type: 'automationRunStarted' }>> {
  return expectResponse(
    await client.request({
      type: 'runAutomation',
      automationId,
      catchUp: false,
    }),
    'automationRunStarted',
  )
}

/**
 * Applies per-automation deltas without replacing the automation catalog.
 *
 * @param client Connected daemon client.
 * @param changes Targeted upsert or remove deltas.
 * @returns The daemon's current automation catalog after applying the deltas.
 */
export async function applyAutomationChanges(
  client: WakuClient,
  changes: AutomationChange[],
): Promise<Automation[]> {
  if (!changes.length) return []
  const response = expectResponse(
    await client.request({ type: 'applyAutomationChanges', changes }),
    'automationChangesApplied',
  )
  return response.automations
}

export async function loadComposerDrafts(client: WakuClient): Promise<ComposerDrafts> {
  const response = expectResponse(
    await client.request({ type: 'loadComposerDrafts' }),
    'composerDrafts',
  )
  return response.drafts
}

export async function applyComposerDraftChanges(
  client: WakuClient,
  changes: ComposerDraftChange[],
): Promise<void> {
  if (!changes.length) return
  expectResponse(await client.request({ type: 'applyComposerDraftChanges', changes }), 'ack')
}

export async function hydrateSession(
  client: WakuClient,
  sessionId: string,
): Promise<AgentSession | null> {
  const response = expectResponse(
    await client.request({ type: 'hydrateSession', sessionId }),
    'session',
  )
  return response.session
}

export async function attachSession(
  client: WakuClient,
  sessionId: string,
): Promise<{ runtimeId: string; supportsSteer: boolean } | null> {
  const response = expectResponse(
    await client.request({ type: 'attachSession' }, sessionId),
    'sessionRuntime',
  )
  return response.runtimeId
    ? { runtimeId: response.runtimeId, supportsSteer: response.supportsSteer }
    : null
}

export async function searchSessionMessages(
  client: WakuClient,
  query: string,
  limit = 40,
): Promise<SessionMessageMatch[]> {
  const response = expectResponse(
    await client.request({ type: 'searchSessionMessages', query, limit }),
    'sessionMessageMatches',
  )
  return response.matches
}

export async function loadDaemonSettings(
  client: WakuClient,
): Promise<DaemonSettings> {
  const response = expectResponse(await client.request({ type: 'getSettings' }), 'settings')
  return {
    ...response.settings,
    provider_binary_overrides: response.settings.provider_binary_overrides ?? {},
  }
}

export async function updateDaemonSettings(
  client: WakuClient,
  settings: DaemonSettings,
): Promise<void> {
  expectResponse(await client.request({ type: 'updateSettings', settings }), 'ack')
}

export async function probeProvider(
  client: WakuClient,
  provider: ProviderKind,
  settings: DaemonSettings,
  options: { discoverModels?: boolean; probeVersion?: boolean } = {},
): Promise<ProviderProbe & { version: string | null }> {
  const { discoverModels = true, probeVersion = true } = options
  const response = expectResponse(
    await client.request({
      type: 'probeProvider',
      provider,
      binaryOverride: settings.provider_binary_overrides?.[provider] ?? null,
      discoverModels,
      probeVersion,
    }),
    'providerProbe',
  )
  return { ...response.probe, version: response.version }
}

export async function loadSkills(
  client: WakuClient,
  projects: Project[],
): Promise<SkillsCatalog> {
  const response = expectResponse(
    await client.request({
      type: 'loadSkills',
      projects: projects.map((project) => [project.name, project.path]),
    }),
    'skillsCatalog',
  )
  return response.catalog
}

export async function setSkillsEnabled(
  client: WakuClient,
  dirs: string[],
  enabled: boolean,
): Promise<void> {
  expectResponse(await client.request({ type: 'setSkillsEnabled', dirs, enabled }), 'ack')
}

export async function trashSkills(client: WakuClient, dirs: string[]): Promise<void> {
  expectResponse(await client.request({ type: 'trashSkills', dirs }), 'ack')
}

export async function loadUsageHistory(
  client: WakuClient,
  window: UsageWindow,
  projects: Project[],
): Promise<UsageHistory> {
  const response = expectResponse(
    await client.request({
      type: 'loadUsageHistory',
      window,
      projectRoots: projects.map((project) => project.path),
    }),
    'usageHistory',
  )
  return response.history
}

export async function fetchPlanUsage(
  client: WakuClient,
  provider: ProviderKind,
  settings: DaemonSettings,
  version: string | null,
): Promise<PlanUsage | null> {
  const response = expectResponse(
    await client.request({
      type: 'fetchPlanUsage',
      provider,
      binaryOverride: settings.provider_binary_overrides?.[provider] ?? null,
      cliVersion: version,
    }),
    'planUsage',
  )
  return response.usage
}

export async function persistSession(
  client: WakuClient,
  session: AgentSession,
  project?: Project,
): Promise<AgentSession> {
  // The daemon merges these keyed records in place. Sending only the changed
  // session keeps streaming checkpoints to one small RPC and cannot reorder
  // projects or overwrite another client's catalog.
  const response = expectResponse(
    await client.request({
      type: 'saveTaskState',
      projects: project ? [project] : [],
      liveSessionIds: [session.id],
      sessions: [session],
    }),
    'taskStateSaved',
  )
  return response.sessions.find((item) => item.id === session.id) ?? session
}

export async function removeSession(
  client: WakuClient,
  sessionId: string,
): Promise<TaskState> {
  expectResponse(
    await client.request({ type: 'removeSession' }, sessionId),
    'ack',
  )
  return loadTaskState(client)
}

export async function listWorkspaceTree(
  client: WakuClient,
  root: string,
  expandedPaths: string[],
): Promise<WorkingTreeEntry[]> {
  const result = await workspaceRequest(client, {
    type: 'listTree',
    root,
    expanded_paths: expandedPaths,
  })
  if (result.type !== 'workingTree') throw new Error('The daemon returned an unexpected file tree')
  return result.entries
}

export async function browseDaemonDirectory(
  client: WakuClient,
  path: string | null,
): Promise<DaemonDirectory> {
  const result = await workspaceRequest(client, {
    type: 'browseDirectory',
    path,
  })
  if (result.type !== 'directory') {
    throw new Error('The daemon returned an unexpected directory response')
  }
  return result
}

export async function readWorkspaceTextFile(
  client: WakuClient,
  root: string,
  relativePath: string,
): Promise<string> {
  const result = await workspaceRequest(client, {
    type: 'readTextFile',
    root,
    relative_path: relativePath,
  })
  if (result.type !== 'textFile') throw new Error('The daemon returned an unexpected file response')
  return result.content
}

export async function writeWorkspaceTextFile(
  client: WakuClient,
  root: string,
  relativePath: string,
  content: string,
): Promise<void> {
  const result = await workspaceRequest(client, {
    type: 'writeTextFile',
    root,
    relative_path: relativePath,
    content,
  })
  if (result.type !== 'ack') throw new Error('The daemon returned an unexpected file response')
}

export async function inspectWorkspaceBranches(
  client: WakuClient,
  cwd: string,
): Promise<BranchSnapshot | null> {
  const result = await workspaceRequest(client, { type: 'inspectBranches', cwd })
  if (result.type !== 'branches') throw new Error('The daemon returned an unexpected Git response')
  return result.snapshot
}

export async function listSessionTurnRefs(
  client: WakuClient,
  cwd: string,
  sessionId: string,
): Promise<number[]> {
  const result = await workspaceRequest(client, {
    type: 'sessionTurnRefs',
    cwd,
    session_id: sessionId,
  })
  if (result.type !== 'turnRefs') {
    throw new Error('The daemon returned an unexpected checkpoint response')
  }
  return result.turn_counts
}

export async function captureTurnStart(
  client: WakuClient,
  cwd: string,
  sessionId: string,
  turnCount: number,
): Promise<void> {
  const result = await workspaceRequest(client, {
    type: 'captureTurnStart',
    cwd,
    session_id: sessionId,
    turn_count: turnCount,
  })
  if (result.type !== 'ack') {
    throw new Error('The daemon returned an unexpected checkpoint response')
  }
}

export async function captureTurnCheckpoint(
  client: WakuClient,
  cwd: string,
  sessionId: string,
  turnCount: number,
): Promise<Checkpoint> {
  const result = await workspaceRequest(client, {
    type: 'captureTurn',
    cwd,
    session_id: sessionId,
    turn_count: turnCount,
  })
  if (result.type !== 'checkpoint') {
    throw new Error('The daemon returned an unexpected checkpoint response')
  }
  return result.checkpoint
}

export async function listComposerFiles(
  client: WakuClient,
  root: string,
  cap = 50_000,
): Promise<FileEntry[]> {
  const result = await workspaceRequest(client, { type: 'listProjectFiles', root, cap })
  if (result.type !== 'projectFiles') {
    throw new Error('The daemon returned an unexpected project-file response')
  }
  return result.entries
}

export async function discoverComposerCommands(
  client: WakuClient,
  provider: ProviderKind,
  projectRoot: string,
): Promise<SlashCommand[]> {
  const result = await workspaceRequest(client, {
    type: 'discoverSlashCommands',
    provider,
    project_root: projectRoot,
  })
  if (result.type !== 'slashCommands') {
    throw new Error('The daemon returned an unexpected slash-command response')
  }
  return result.commands
}

export async function checkoutWorkspaceBranch(
  client: WakuClient,
  cwd: string,
  branch: string,
  create = false,
): Promise<BranchSnapshot> {
  const result = await workspaceRequest(client, {
    type: 'checkoutBranch',
    cwd,
    branch,
    create,
  })
  if (result.type !== 'branchChanged') throw new Error('The daemon returned an unexpected branch response')
  return result.snapshot
}

export async function collectWorkspaceDiff(
  client: WakuClient,
  cwd: string,
  source: ReviewDiffSource = 'uncommitted',
): Promise<ReviewDiffData> {
  const result = await workspaceRequest(client, {
    type: 'collectReviewDiff',
    cwd,
    source,
  })
  if (result.type !== 'reviewDiff') throw new Error('The daemon returned an unexpected diff response')
  return result.data
}

export async function inspectWorkspaceCommit(
  client: WakuClient,
  cwd: string,
): Promise<CommitSnapshot> {
  const result = await workspaceRequest(client, { type: 'inspectCommit', cwd })
  if (result.type !== 'commitSnapshot') {
    throw new Error('The daemon returned an unexpected commit response')
  }
  return result.snapshot
}

export async function generateWorkspaceCommitMessage(
  client: WakuClient,
  cwd: string,
  includeUnstaged: boolean,
  invocation: AgentInvocation,
): Promise<string> {
  const result = await workspaceRequest(client, {
    type: 'generateCommitMessage',
    cwd,
    include_unstaged: includeUnstaged,
    invocation,
  })
  if (result.type !== 'commitMessage') {
    throw new Error('The daemon returned an unexpected commit-message response')
  }
  return result.message
}

export async function commitWorkspace(
  client: WakuClient,
  cwd: string,
  message: string,
  includeUnstaged: boolean,
  push: boolean,
): Promise<void> {
  const result = await workspaceRequest(client, {
    type: 'commit',
    cwd,
    message,
    include_unstaged: includeUnstaged,
    push,
  })
  if (result.type !== 'ack') throw new Error('The daemon returned an unexpected commit response')
}

export async function pushWorkspace(client: WakuClient, cwd: string): Promise<void> {
  const result = await workspaceRequest(client, { type: 'push', cwd })
  if (result.type !== 'ack') throw new Error('The daemon returned an unexpected push response')
}

async function workspaceRequest(
  client: WakuClient,
  operation: WorkspaceOperation,
): Promise<WorkspaceResult> {
  const response = expectResponse(
    await client.request({ type: 'workspace', operation }),
    'workspace',
  )
  return response.result
}

export async function createProjectlessWorkspace(
  client: WakuClient,
): Promise<string> {
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: { type: 'createProjectlessWorkspace', prompt: null },
    }),
    'workspace',
  )
  if (response.result.type !== 'projectlessWorkspace') {
    throw new Error('The daemon returned an unexpected workspace result')
  }
  return response.result.cwd
}

export async function materializeWorktree(
  client: WakuClient,
  session: AgentSession,
  project: Project,
  prompt: string,
): Promise<AgentSession> {
  if (session.workspace?.kind !== 'newWorktree') return session
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: {
        type: 'createWorktree',
        project_path: project.path,
        project_id: project.id,
        session_id: session.id,
        prompt,
        base_branch: session.workspace.baseBranch ?? null,
      },
    }),
    'workspace',
  )
  if (response.result.type !== 'worktreeCreated') {
    throw new Error('The daemon returned an unexpected worktree result')
  }
  return {
    ...session,
    workspace: {
      kind: 'worktree',
      path: response.result.worktree.path,
      branch: response.result.worktree.branch,
    },
  }
}

export function createProject(path: string): Project {
  const input = path.trim()
  if (!input.startsWith('/') && !/^[a-z]:[\\/]/i.test(input)) {
    throw new Error('Enter an absolute path on the daemon host')
  }
  const normalized = input === '/' ? input : input.replace(/[\\/]+$/, '')
  const name = normalized.split(/[\\/]/).filter(Boolean).at(-1) ?? 'Project'
  return {
    id: crypto.randomUUID(),
    name,
    path: normalized,
    created_at: unixTime(),
  }
}

export async function persistProject(
  client: WakuClient,
  candidate: Project,
): Promise<{ project: Project; taskState: TaskState }> {
  const current = await loadTaskState(client)
  const existing = current.projects.find((project) => project.path === candidate.path)
  if (existing) return { project: existing, taskState: current }

  const projects = [...current.projects, candidate]
  expectResponse(
    await client.request({
      type: 'saveTaskState',
      projects,
      liveSessionIds: current.sessions.map((session) => session.id),
      sessions: [],
    }),
    'taskStateSaved',
  )
  return {
    project: candidate,
    taskState: { ...current, projects },
  }
}

export function selectableProjects(projects: Project[], selected?: Project): Project[] {
  const choices = selected
    ? [selected, ...projects.filter((project) => project.id !== selected.id)]
    : projects
  let hasProjectless = false
  return choices.filter((project, index) => {
    if (choices.findIndex((candidate) => candidate.id === project.id) !== index) return false
    if (project.name !== 'No project') return true
    if (hasProjectless) return false
    hasProjectless = true
    return true
  })
}

export function createSession(
  projectId: string,
  provider: ProviderKind,
  isolated: boolean,
): AgentSession {
  const now = unixTime()
  return {
    id: crypto.randomUUID(),
    title: 'New task',
    auto_title: null,
    project_id: projectId,
    workspace: isolated ? { kind: 'newWorktree' } : { kind: 'local' },
    provider,
    model: null,
    runtime_mode: 'fullAccess',
    interaction_mode: 'build',
    reasoning_effort: null,
    service_tier: null,
    context_window: null,
    agent_preset: null,
    status: 'idle',
    created_at: now,
    updated_at: now,
    last_reply_at: null,
    provider_cursor: null,
    available_commands: [],
    context_usage: null,
    provider_session_id: null,
    messages: [],
    transcript_blocks: [],
    turns: [],
    queued_messages: [],
  }
}

export function beginTurn(
  session: AgentSession,
  prompt: string,
  attachments: MessageAttachment[] = [],
): AgentSession {
  const now = unixTime()
  const turnId = crypto.randomUUID()
  const visiblePrompt = prompt.trim()
  const mentions = attachments.map((attachment) => `@${attachment.mention}`).join(' ')
  const providerPrompt = [visiblePrompt, mentions].filter(Boolean).join(' ')
  const autoTitle =
    session.messages.length === 0 && session.title === 'New task' && !session.auto_title
      ? promptTitle(visiblePrompt || attachments[0]?.name || '')
      : session.auto_title
  return {
    ...session,
    auto_title: autoTitle,
    status: 'connecting',
    updated_at: now,
    last_reply_at: now,
    messages: [
      ...session.messages,
      {
        id: crypto.randomUUID(),
        turn_id: turnId,
        role: 'user',
        content: providerPrompt,
        display_content: attachments.length ? visiblePrompt : null,
        attachments,
        created_at: now,
        streaming: false,
      },
    ],
    turns: [
      ...session.turns,
      {
        id: turnId,
        turn_count: session.turns.length + 1,
        status: 'running',
        provider_turn_started: false,
        provider_resume_at: null,
        started_at: now,
        completed_at: null,
        checkpoint: null,
      },
    ],
  }
}

export function displayTitle(session: AgentSession): string {
  if (session.title !== 'New task' && session.title.trim()) return session.title
  return session.auto_title?.trim() || 'New Task'
}

export function sessionCwd(session: AgentSession, project: Project): string {
  return session.workspace?.kind === 'worktree' ? session.workspace.path : project.path
}

export function unixTime() {
  return Math.floor(Date.now() / 1_000)
}

function promptTitle(prompt: string): string | null {
  let title = prompt.trim().split(/\s+/).slice(0, 7).join(' ')
  if (!title) return null
  if ([...title].length > 54) title = `${[...title].slice(0, 53).join('')}…`
  return title
}

function upsertProject(projects: Project[], project?: Project): Project[] {
  if (!project) return projects
  const found = projects.some((item) => item.id === project.id)
  return found
    ? projects.map((item) => (item.id === project.id ? project : item))
    : [...projects, project]
}

function expectResponse<T extends ResponsePayload['type']>(
  response: ResponsePayload,
  type: T,
): Extract<ResponsePayload, { type: T }> {
  if (response.type !== type) {
    throw new Error(`Expected ${type}, received ${response.type}`)
  }
  return response as Extract<ResponsePayload, { type: T }>
}
