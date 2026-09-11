import type {
  AgentSession,
  BranchSnapshot,
  ComposerDraftChange,
  DaemonSettings,
  PlanUsage,
  Project,
  ProviderKind,
  ProviderProbe,
  ProviderResumeCursor,
  ProviderSessionHistory,
  ProviderSessionSummary,
  ResponsePayload,
  ReviewDiffData,
  ReviewDiffSource,
  RuntimeMode,
  SessionMessageMatch,
  SkillsCatalog,
  SlashCommand,
  UsageHistory,
  UsageWindow,
  WakuClient,
  WorkingTreeEntry,
  WorkspaceResult,
} from '@waku/client';

export type TaskState = Extract<ResponsePayload, { type: 'taskState' }>;
export type DaemonDirectory = Extract<WorkspaceResult, { type: 'directory' }>;

export const daemonKeys = {
  taskState: (profileId: string) => ['daemon', profileId, 'task-state'] as const,
  session: (profileId: string, sessionId: string) => [
    'daemon',
    profileId,
    'session',
    sessionId,
  ] as const,
  settings: (profileId: string) => ['daemon', profileId, 'settings'] as const,
  provider: (profileId: string, provider: ProviderKind) => [
    'daemon',
    profileId,
    'provider',
    provider,
  ] as const,
  skills: (profileId: string) => ['daemon', profileId, 'skills'] as const,
  planUsage: (profileId: string, provider: ProviderKind) => [
    'daemon',
    profileId,
    'plan-usage',
    provider,
  ] as const,
  usage: (profileId: string, window: UsageWindow) => [
    'daemon',
    profileId,
    'usage',
    JSON.stringify(window),
  ] as const,
  messageSearch: (profileId: string, query: string) => [
    'daemon',
    profileId,
    'message-search',
    query,
  ] as const,
  slashCommands: (
    profileId: string,
    provider: ProviderKind,
    cwd: string,
    binaryOverride: string | null = null,
  ) => [
    'daemon',
    profileId,
    'slash-commands',
    provider,
    cwd,
    binaryOverride,
  ] as const,
  directory: (profileId: string, path: string | null) => [
    'daemon',
    profileId,
    'directory',
    path ?? 'home',
  ] as const,
  branches: (profileId: string, cwd: string) => [
    'daemon',
    profileId,
    'branches',
    cwd,
  ] as const,
  workspaceTree: (profileId: string, root: string, expandedPaths: string[]) => [
    'daemon',
    profileId,
    'workspace-tree',
    root,
    expandedPaths,
  ] as const,
  workspaceFile: (profileId: string, root: string, relativePath: string) => [
    'daemon',
    profileId,
    'workspace-file',
    root,
    relativePath,
  ] as const,
  workspaceDiff: (profileId: string, root: string, source: ReviewDiffSource) => [
    'daemon',
    profileId,
    'workspace-diff',
    root,
    source,
  ] as const,
};

export async function loadTaskState(client: WakuClient): Promise<TaskState> {
  return expectResponse(await client.request({ type: 'loadTaskState' }), 'taskState');
}

export async function hydrateSession(
  client: WakuClient,
  sessionId: string,
): Promise<AgentSession | null> {
  const response = expectResponse(
    await client.request({ type: 'hydrateSession', sessionId }),
    'session',
  );
  return response.session;
}

export async function attachDaemonSession(
  client: WakuClient,
  sessionId: string,
): Promise<{ runtimeId: string; supportsSteer: boolean } | null> {
  const response = expectResponse(
    await client.request({ type: 'attachSession' }, sessionId),
    'sessionRuntime',
  );
  return response.runtimeId
    ? { runtimeId: response.runtimeId, supportsSteer: response.supportsSteer }
    : null;
}

export async function loadDaemonSettings(client: WakuClient): Promise<DaemonSettings> {
  const response = expectResponse(await client.request({ type: 'getSettings' }), 'settings');
  return {
    ...response.settings,
    provider_binary_overrides: response.settings.provider_binary_overrides ?? {},
  };
}

/** Scans the provider transcripts under the given project roots. The window
 * selects the range; the daemon prices it, so the client only renders. */
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
  );
  return response.history;
}

/** Account-level plan limits, which the provider CLI reports rather than
 * anything derivable from local transcripts. Null when unsupported. */
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
  );
  return response.usage;
}

/** Skills are discovered on the daemon host, across the user's shared
 * directory and each project. */
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
  );
  return response.catalog;
}

export async function setSkillsEnabled(
  client: WakuClient,
  dirs: string[],
  enabled: boolean,
): Promise<void> {
  expectResponse(await client.request({ type: 'setSkillsEnabled', dirs, enabled }), 'ack');
}

/** Moves a skill's directory to the trash on the daemon host — recoverable
 * there, but not from the phone, hence the confirm upstream. */
export async function trashSkills(client: WakuClient, dirs: string[]): Promise<void> {
  expectResponse(await client.request({ type: 'trashSkills', dirs }), 'ack');
}

/** Slash commands the project, user, and skills define for a provider, as
 * discovered on the daemon host. Provider-reported commands arrive with the
 * session instead; the composer merges the two. */
export async function discoverComposerCommands(
  client: WakuClient,
  provider: ProviderKind,
  projectRoot: string,
  binaryOverride: string | null,
): Promise<SlashCommand[]> {
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: {
        type: 'discoverSlashCommands',
        provider,
        project_root: projectRoot,
        binary_override: binaryOverride,
      },
    }),
    'workspace',
  );
  if (response.result.type !== 'slashCommands') {
    throw new Error('The daemon returned an unexpected slash-command response');
  }
  return response.result.commands;
}

/** Full-text search across every transcript the daemon has. Titles are
 * matched client-side; this is the only way in by content. */
export async function searchSessionMessages(
  client: WakuClient,
  query: string,
  limit = 40,
): Promise<SessionMessageMatch[]> {
  const response = expectResponse(
    await client.request({ type: 'searchSessionMessages', query, limit }),
    'sessionMessageMatches',
  );
  return response.matches;
}

/** Settings are daemon-wide, so a phone writing them changes the desktop too.
 * The full object is sent: the daemon replaces rather than merges. */
export async function updateDaemonSettings(
  client: WakuClient,
  settings: DaemonSettings,
): Promise<void> {
  expectResponse(await client.request({ type: 'updateSettings', settings }), 'ack');
}

export async function probeProvider(
  client: WakuClient,
  provider: ProviderKind,
  settings: DaemonSettings,
  options: { discoverModels?: boolean; probeVersion?: boolean } = {},
): Promise<ProviderProbe & { version: string | null }> {
  const response = expectResponse(
    await client.request({
      type: 'probeProvider',
      provider,
      binaryOverride: settings.provider_binary_overrides?.[provider] ?? null,
      discoverModels: options.discoverModels ?? true,
      probeVersion: options.probeVersion ?? false,
    }),
    'providerProbe',
  );
  return { ...response.probe, version: response.version ?? null };
}

export async function browseDaemonDirectory(
  client: WakuClient,
  path: string | null,
): Promise<DaemonDirectory> {
  const response = expectResponse(
    await client.request({ type: 'workspace', operation: { type: 'browseDirectory', path } }),
    'workspace',
  );
  if (response.result.type !== 'directory') {
    throw new Error('The daemon returned an unexpected directory response');
  }
  return response.result;
}

export function createProject(
  rawPath: string,
  id: string,
  createdAt = Math.floor(Date.now() / 1_000),
): Project {
  const input = rawPath.trim();
  if (!input.startsWith('/') && !/^[a-z]:[\\/]/i.test(input)) {
    throw new Error('Enter an absolute path on the daemon host');
  }
  const path = input === '/' ? input : input.replace(/[\\/]+$/, '');
  const name = path.split(/[\\/]/).filter(Boolean).at(-1) ?? 'Project';
  return { id, name, path, created_at: createdAt };
}

export async function persistProject(
  client: WakuClient,
  candidate: Project,
): Promise<{ project: Project; taskState: TaskState }> {
  const current = await loadTaskState(client);
  const existing = current.projects.find((project) => project.path === candidate.path);
  if (existing) return { project: existing, taskState: current };
  const projects = [...current.projects, candidate];
  expectResponse(
    await client.request({
      type: 'saveTaskState',
      projects,
      liveSessionIds: current.sessions.map((session) => session.id),
      sessions: [],
    }),
    'taskStateSaved',
  );
  return { project: candidate, taskState: { ...current, projects } };
}

export async function createProjectlessWorkspace(client: WakuClient): Promise<string> {
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: { type: 'createProjectlessWorkspace', prompt: null },
    }),
    'workspace',
  );
  if (response.result.type !== 'projectlessWorkspace') {
    throw new Error('The daemon returned an unexpected workspace response');
  }
  return response.result.cwd;
}

export async function materializeWorktree(
  client: WakuClient,
  session: AgentSession,
  projectPath: string,
  prompt: string,
): Promise<AgentSession> {
  if (session.workspace?.kind !== 'newWorktree') return session;
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: {
        type: 'createWorktree',
        project_path: projectPath,
        project_id: session.project_id,
        session_id: session.id,
        prompt,
        base_branch: session.workspace.baseBranch ?? null,
      },
    }),
    'workspace',
  );
  if (response.result.type !== 'worktreeCreated') {
    throw new Error('The daemon returned an unexpected worktree result');
  }
  return {
    ...session,
    workspace: {
      kind: 'worktree',
      path: response.result.worktree.path,
      branch: response.result.worktree.branch,
    },
  };
}

export async function loadComposerDrafts(
  client: WakuClient,
): Promise<Extract<ResponsePayload, { type: 'composerDrafts' }>['drafts']> {
  const response = expectResponse(
    await client.request({ type: 'loadComposerDrafts' }),
    'composerDrafts',
  );
  return response.drafts;
}

export async function applyComposerDraftChanges(
  client: WakuClient,
  changes: ComposerDraftChange[],
): Promise<void> {
  expectResponse(
    await client.request({ type: 'applyComposerDraftChanges', changes }),
    'ack',
  );
}

export async function inspectBranches(
  client: WakuClient,
  cwd: string,
): Promise<BranchSnapshot | null> {
  const response = expectResponse(
    await client.request({ type: 'workspace', operation: { type: 'inspectBranches', cwd } }),
    'workspace',
  );
  if (response.result.type !== 'branches') {
    throw new Error('The daemon returned an unexpected branches response');
  }
  return response.result.snapshot;
}

export async function listWorkspaceTree(
  client: WakuClient,
  root: string,
  expandedPaths: string[],
): Promise<WorkingTreeEntry[]> {
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: { type: 'listTree', root, expanded_paths: expandedPaths },
    }),
    'workspace',
  );
  if (response.result.type !== 'workingTree') {
    throw new Error('The daemon returned an unexpected file tree');
  }
  return response.result.entries;
}

export async function readWorkspaceTextFile(
  client: WakuClient,
  root: string,
  relativePath: string,
): Promise<string> {
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: { type: 'readTextFile', root, relative_path: relativePath },
    }),
    'workspace',
  );
  if (response.result.type !== 'textFile') {
    throw new Error('The daemon returned an unexpected file response');
  }
  return response.result.content;
}

export async function collectWorkspaceDiff(
  client: WakuClient,
  cwd: string,
  source: ReviewDiffSource = 'uncommitted',
): Promise<ReviewDiffData> {
  const response = expectResponse(
    await client.request({
      type: 'workspace',
      operation: { type: 'collectReviewDiff', cwd, source },
    }),
    'workspace',
  );
  if (response.result.type !== 'reviewDiff') {
    throw new Error('The daemon returned an unexpected diff response');
  }
  return response.result.data;
}

export async function removeDaemonSession(
  client: WakuClient,
  sessionId: string,
): Promise<void> {
  expectResponse(await client.request({ type: 'removeSession' }, sessionId), 'ack');
}

export async function persistSession(
  client: WakuClient,
  session: AgentSession,
): Promise<AgentSession> {
  const response = expectResponse(
    await client.request({
      type: 'saveTaskState',
      projects: [],
      liveSessionIds: [session.id],
      sessions: [session],
    }),
    'taskStateSaved',
  );
  return response.sessions.find((item) => item.id === session.id) ?? session;
}

export type SessionRewound = Extract<ResponsePayload, { type: 'sessionRewound' }>;
export type SessionForked = Extract<ResponsePayload, { type: 'sessionForked' }>;

/**
 * Drops every turn after `turnCount` and rewinds the workspace to that point.
 *
 * The daemon answers with the truncated session rather than an ack, because a
 * rewind also rewinds checkpoints the client cannot reconstruct on its own.
 * `runtimeId` is passed along so the daemon rewinds the runtime the client is
 * actually following, the way the web client does.
 */
export async function rewindSessionToMessage(
  client: WakuClient,
  sessionId: string,
  runtimeId: string | undefined,
  turnCount: number,
): Promise<SessionRewound> {
  return expectResponse(
    await client.request(
      { type: 'rewindSessionToMessage', turnCount },
      sessionId,
      runtimeId,
    ),
    'sessionRewound',
  );
}

/** Copies the task up to `turnCount` turns into a new session, leaving this
 * one untouched. */
export async function forkSessionFromResponse(
  client: WakuClient,
  sessionId: string,
  runtimeId: string | undefined,
  turnCount: number,
): Promise<SessionForked> {
  return expectResponse(
    await client.request(
      { type: 'forkSessionFromResponse', turnCount },
      sessionId,
      runtimeId,
    ),
    'sessionForked',
  );
}

/** Sessions an agent CLI started on the daemon host, but Waku has not pulled
 * in yet. `/resume` lets the user adopt one as a Waku task. */
export async function listProviderSessions(
  client: WakuClient,
  provider: ProviderKind,
  limit = 250,
): Promise<ProviderSessionSummary[]> {
  const response = expectResponse(
    await client.request({ type: 'listProviderSessions', provider, limit }),
    'providerSessions',
  );
  return response.sessions;
}

/** Full messages and turns for a picked provider session, fetched only after
 * the user chooses one. */
export async function loadProviderSessionHistory(
  client: WakuClient,
  summary: ProviderSessionSummary,
): Promise<ProviderSessionHistory> {
  const response = expectResponse(
    await client.request({
      type: 'loadProviderSession',
      cursor: summary.cursor,
      cwd: summary.cwd,
    }),
    'providerSessionHistory',
  );
  return response.history;
}

export function providerSessionNativeId(cursor: ProviderResumeCursor): string {
  return cursor.provider === 'amp' || cursor.provider === 'codex'
    ? cursor.threadId
    : cursor.sessionId;
}

/** The daemon may already track this provider session; rewording the native id
 * keeps a resume from creating a second Waku task for one CLI session. */
export function sameProviderSession(
  left: ProviderResumeCursor,
  right: ProviderResumeCursor,
): boolean {
  return left.provider === right.provider
    && providerSessionNativeId(left) === providerSessionNativeId(right);
}

/** Merges a fresh provider-native import into a tracked task. The provider CLI
 * or another client may have continued the conversation since the snapshot was
 * stored, so messages and turns are replaced wholesale while Waku-owned fields
 * (title, model, workspace) stay untouched. Returns the same session object
 * when the import adds nothing. Mirrors `AgentSession::refresh_from_provider_history`. */
export function refreshTrackedSession(
  session: AgentSession,
  history: ProviderSessionHistory,
): AgentSession {
  const hasHistory = history.messages.length > 0 || history.turns.length > 0;
  if (!hasHistory) return session;
  const newest = Math.max(
    ...history.turns.map((turn) => turn.completed_at ?? 0),
    ...history.messages.map((message) => message.created_at),
  );
  const countGrew = history.messages.length > session.messages.length;
  if (!countGrew && newest <= (session.last_reply_at ?? 0)) return session;

  return {
    ...session,
    messages: history.messages,
    turns: history.turns,
    transcript_blocks: [],
    queued_messages: [],
    updated_at: Math.max(session.updated_at, newest),
    last_reply_at: newest > 0 ? Math.max(session.last_reply_at ?? 0, newest) : session.last_reply_at,
  };
}

/** Builds the task Waku opens for an adopted provider session. The history is
 * copied verbatim; the daemon replays it from `provider_cursor` on the next
 * turn rather than re-running the CLI conversation. */
export function createResumedSession(
  projectId: string,
  summary: ProviderSessionSummary,
  history: ProviderSessionHistory,
  runtimeMode: RuntimeMode = 'fullAccess',
): AgentSession {
  const now = Math.floor(Date.now() / 1_000);
  const createdAt = summary.created_at || now;
  const updatedAt = Math.max(summary.updated_at, createdAt);
  const hasHistory = history.messages.length > 0 || history.turns.length > 0;
  return {
    id: crypto.randomUUID(),
    title: 'New task',
    auto_title: summary.title,
    project_id: projectId,
    workspace: { kind: 'local' },
    provider: summary.cursor.provider,
    model: null,
    runtime_mode: runtimeMode,
    reasoning_effort: null,
    service_tier: null,
    context_window: null,
    agent_preset: null,
    status: 'idle',
    created_at: createdAt,
    updated_at: updatedAt,
    last_reply_at: hasHistory ? updatedAt : null,
    provider_cursor: summary.cursor,
    available_commands: [],
    context_usage: null,
    provider_session_id: null,
    messages: history.messages,
    transcript_blocks: [],
    turns: history.turns,
    queued_messages: [],
  };
}

function expectResponse<T extends ResponsePayload['type']>(
  response: ResponsePayload,
  expected: T,
): Extract<ResponsePayload, { type: T }> {
  if (response.type !== expected) {
    throw new Error(`Expected daemon response ${expected}, received ${response.type}`);
  }
  return response as Extract<ResponsePayload, { type: T }>;
}
