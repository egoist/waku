import type {
  AgentSession,
  MessageAttachment,
  Project,
  ProviderKind,
  ProviderSessionHistory,
  ProviderSessionSummary,
  RuntimeMode,
  SequencedEvent,
} from '@waku/client';

export interface MobileRuntimeClock {
  nowSeconds: () => number;
  randomUUID: () => string;
}

export interface NewSessionOptions {
  model?: string | null;
  reasoningEffort?: string | null;
  serviceTier?: string | null;
  contextWindow?: string | null;
  runtimeMode?: RuntimeMode;
  /** Base branch for an isolated worktree; null means the project default. */
  baseBranch?: string | null;
}

export function beginTurn(
  session: AgentSession,
  prompt: string,
  clock: MobileRuntimeClock,
  attachments: MessageAttachment[] = [],
  providerPromptOverride?: string,
): AgentSession {
  const now = clock.nowSeconds();
  const turnId = clock.randomUUID();
  const visiblePrompt = prompt.trim();
  const providerPrompt = providerPromptOverride === undefined
    ? providerPromptForSubmission(visiblePrompt, attachments)
    : providerPromptOverride.trim();
  return {
    ...session,
    auto_title:
      session.messages.length === 0 && session.title === 'New task' && !session.auto_title
        ? promptTitle(visiblePrompt || attachments[0]?.name || '')
        : session.auto_title,
    status: 'connecting',
    updated_at: now,
    last_reply_at: now,
    messages: [
      ...session.messages,
      {
        id: clock.randomUUID(),
        turn_id: turnId,
        role: 'user',
        content: providerPrompt,
        display_content:
          attachments.length || providerPrompt !== visiblePrompt ? visiblePrompt : null,
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
  };
}

export function createSession(
  projectId: string,
  provider: ProviderKind,
  isolated: boolean,
  clock: MobileRuntimeClock,
  options: NewSessionOptions = {},
): AgentSession {
  const now = clock.nowSeconds();
  return {
    id: clock.randomUUID(),
    title: 'New task',
    auto_title: null,
    project_id: projectId,
    workspace: isolated
      ? { kind: 'newWorktree', baseBranch: options.baseBranch ?? null }
      : { kind: 'local' },
    provider,
    model: options.model ?? null,
    runtime_mode: options.runtimeMode ?? 'fullAccess',
    reasoning_effort: options.reasoningEffort ?? null,
    service_tier: options.serviceTier ?? null,
    context_window: options.contextWindow ?? null,
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
  };
}

export function createResumedSession(
  projectId: string,
  summary: ProviderSessionSummary,
  history: ProviderSessionHistory,
  runtimeMode: RuntimeMode,
  clock: MobileRuntimeClock,
): AgentSession {
  const createdAt = summary.created_at || clock.nowSeconds();
  const updatedAt = Math.max(summary.updated_at, createdAt);
  return {
    ...createSession(projectId, summary.cursor.provider, false, clock, { runtimeMode }),
    auto_title: summary.title,
    provider_cursor: summary.cursor,
    created_at: createdAt,
    updated_at: updatedAt,
    last_reply_at: history.messages.length || history.turns.length ? updatedAt : null,
    messages: history.messages,
    turns: history.turns,
  };
}

/** Mirrors the web client's queueSubmission: a prompt sent while a turn is
 * live becomes a persisted QueuedMessage that drains after the turn settles. */
export function queueSubmission(
  session: AgentSession,
  prompt: string,
  clock: MobileRuntimeClock,
  attachments: MessageAttachment[] = [],
  providerPromptOverride?: string,
): AgentSession {
  const now = clock.nowSeconds();
  const visiblePrompt = prompt.trim();
  const providerPrompt = providerPromptOverride === undefined
    ? providerPromptForSubmission(visiblePrompt, attachments)
    : providerPromptOverride.trim();
  return {
    ...session,
    updated_at: now,
    queued_messages: [
      ...(session.queued_messages ?? []),
      {
        id: clock.randomUUID(),
        content: providerPrompt,
        display_content:
          attachments.length || providerPrompt !== visiblePrompt ? visiblePrompt : null,
        attachments,
        created_at: now,
      },
    ],
  };
}

export function providerPromptForSubmission(
  prompt: string,
  attachments: MessageAttachment[],
): string {
  return [
    prompt.trim(),
    attachments.map((attachment) => `@${attachment.mention}`).join(' '),
  ].filter(Boolean).join(' ');
}

/** The ids beginTurn gave the running turn and the user message that opened
 * it. They are sent with the prompt so the daemon can publish the same
 * identity to every other client attached to the runtime. */
export function submittedTurnIdentity(
  session: AgentSession,
): { turnId: string | null; messageId: string | null } {
  const turn = session.turns.at(-1);
  if (!turn || turn.status !== 'running') return { turnId: null, messageId: null };
  const message = session.messages.find(
    (candidate) => candidate.turn_id === turn.id && candidate.role === 'user',
  );
  return { turnId: turn.id, messageId: message?.id ?? null };
}

export function sessionBusy(session: Pick<AgentSession, 'status'>): boolean {
  return (
    session.status === 'connecting'
    || session.status === 'working'
    || session.status === 'waiting'
    || session.status === 'background'
  );
}

/** A list projection has no turns, so its status is the best available
 * signal. Once hydrated, a settled latest turn wins over a lagging status. */
export function sessionIsRunning(
  session: Pick<AgentSession, 'status' | 'turns'>,
): boolean {
  if (session.status !== 'connecting' && session.status !== 'working') return false;
  const latestTurn = session.turns.at(-1);
  return !latestTurn || latestTurn.status === 'running';
}

export interface SessionOptionChanges {
  model?: string | null;
  reasoningEffort?: string | null;
  serviceTier?: string | null;
  contextWindow?: string | null;
  runtimeMode?: RuntimeMode;
}

export function applySessionOptions(
  session: AgentSession,
  changes: SessionOptionChanges,
  clock: MobileRuntimeClock,
): AgentSession {
  return {
    ...session,
    model: changes.model !== undefined ? changes.model : session.model,
    reasoning_effort:
      changes.reasoningEffort !== undefined ? changes.reasoningEffort : session.reasoning_effort,
    service_tier:
      changes.serviceTier !== undefined ? changes.serviceTier : session.service_tier,
    context_window:
      changes.contextWindow !== undefined ? changes.contextWindow : session.context_window,
    runtime_mode: changes.runtimeMode ?? session.runtime_mode,
    updated_at: clock.nowSeconds(),
  };
}

export function sessionCwd(session: AgentSession, project: Project): string {
  return session.workspace?.kind === 'worktree' ? session.workspace.path : project.path;
}

export function runtimeEventAlreadyApplied(session: AgentSession, event: SequencedEvent): boolean {
  const cursor = session.runtime_event_cursor;
  return Boolean(
    cursor && cursor.runtime_id === event.runtimeId && cursor.epoch === event.epoch &&
      cursor.sequence >= event.sequence,
  );
}

export function shouldApplyRuntimeEvent(
  session: AgentSession,
  event: SequencedEvent,
): boolean {
  if (!runtimeEventAlreadyApplied(session, event)) return true;
  return session.status === 'waiting' && (
    event.event.kind === 'permission' || event.event.kind === 'userInputRequested'
  );
}

function promptTitle(prompt: string): string | null {
  let title = prompt.split(/\s+/u).filter(Boolean).slice(0, 7).join(' ');
  if (!title) return null;
  if ([...title].length > 54) title = `${[...title].slice(0, 53).join('')}…`;
  return title;
}
