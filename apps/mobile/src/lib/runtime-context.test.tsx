import { afterEach, describe, expect, mock, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AgentSession, Command, ResponsePayload, SequencedEvent, WakuClient } from '@waku/client';
import { reduceRuntimeEvent } from '@waku/client/event-reducer';
import { renderToStaticMarkup } from 'react-dom/server';

import { daemonKeys, hydrateSession } from './daemon-api';
import { beginTurn, createSession } from './mobile-runtime';

let activeDaemon: { activeProfile: { id: string }; client: WakuClient; phase: 'connected' };
mock.module('./daemon-context', () => ({ useDaemon: () => activeDaemon }));
mock.module('expo-crypto', () => ({ randomUUID: () => crypto.randomUUID() }));
mock.module('./composer-preferences-store', () => ({ persistentStorageSync: () => null }));

const { RuntimeProvider, useRuntime } = await import('./runtime-context');
const cleanups: Array<() => Promise<void>> = [];

afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) await cleanup();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function fixture(options: { attached?: boolean } = {}) {
  let nextId = 0;
  const clock = {
    nowSeconds: () => 100,
    nowMillis: () => 100_000,
    randomUUID: () => `id-${++nextId}`,
  };
  let sequence = 0;
  let history = createSession('project', 'codex', false, clock);
  function event(kind: string, payload: SequencedEvent['event']['payload']): SequencedEvent {
    return {
      sessionId: history.id,
      runtimeId: 'runtime',
      epoch: 'epoch',
      sequence: ++sequence,
      event: { kind, payload },
    };
  }
  history = beginTurn(history, 'Previous request', clock);
  history = reduceRuntimeEvent(history, event('turnStarted', null), clock).session;
  history = reduceRuntimeEvent(history, event('textDelta', 'Previous answer'), clock).session;
  history = reduceRuntimeEvent(history, event('turnFinished', { success: true }), clock).session;
  history = beginTurn(history, 'Current request', clock);
  history = reduceRuntimeEvent(history, event('turnStarted', null), clock).session;
  history = reduceRuntimeEvent(history, event('textDelta', 'Streaming response'), clock).session;
  const skeleton: AgentSession = {
    ...history,
    messages: [],
    turns: [],
    transcript_blocks: [],
    runtime_event_cursor: null,
  };
  const hydration = deferred<AgentSession | null>();
  const commands: Command[] = [];
  let listener: ((event: SequencedEvent) => void) | undefined;
  const client = {
    connected: true,
    request: async (command: Command): Promise<ResponsePayload> => {
      commands.push(command);
      switch (command.type) {
        case 'hydrateSession':
          return { type: 'session', session: await hydration.promise };
        case 'attachSession':
          return { type: 'sessionRuntime', runtimeId: options.attached === false ? null : 'runtime', supportsSteer: true };
        case 'getSettings':
          return { type: 'settings', settings: {
            provider_binary_overrides: {}, disabled_providers: [],
            computer_use_enabled: false, computer_use_allowed_apps: [],
          } };
        case 'loadTaskState':
          return {
            type: 'taskState',
            defaultCwd: '/repo',
            projectlessRoot: null,
            projects: [{ id: 'project', name: 'Project', path: '/repo', created_at: 0 }],
            sessions: [history],
          };
        case 'probeProvider':
          return {
            type: 'providerProbe', version: null,
            probe: { provider: 'codex', installed: true, path: '/bin/codex', models: [], agent_presets: [] },
          };
        case 'start':
          return { type: 'started', supportsSteer: true };
        case 'saveTaskState':
          return { type: 'taskStateSaved', sessions: command.sessions };
        default:
          return { type: 'ack' };
      }
    },
    subscribe: (_sessionId: string, _runtimeId: string, next: typeof listener) => {
      listener = next;
      return () => { listener = undefined; };
    },
  } as unknown as WakuClient;
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const key = daemonKeys.session('profile', history.id);
  activeDaemon = { activeProfile: { id: 'profile' }, client, phase: 'connected' };
  let runtime!: ReturnType<typeof useRuntime>;
  function CaptureRuntime() {
    runtime = useRuntime();
    return null;
  }
  // Capture the real runtime callbacks without mounting a native view tree.
  // The tests exercise their refs, subscriptions and actual query-cache writes.
  renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <RuntimeProvider><CaptureRuntime /></RuntimeProvider>
    </QueryClientProvider>,
  );
  cleanups.push(async () => {
    await runtime.deleteSession(history.id);
    queryClient.clear();
  });
  return {
    history, skeleton, hydration, commands, client, queryClient, key, runtime,
    emit(kind: string, payload: SequencedEvent['event']['payload']) {
      if (!listener) throw new Error('The runtime is not subscribed');
      listener(event(kind, payload));
    },
    current: () => queryClient.getQueryData<AgentSession>(key)!,
  };
}

describe('mobile runtime history', () => {
  test('creates a task with expanded provider text while retaining its slash command for display', async () => {
    const f = fixture({ attached: false });
    const session = await f.runtime.createTask('project', 'codex', false, '/deploy production', {}, '$deploy production');
    cleanups.push(() => f.runtime.deleteSession(session.id));
    expect(f.commands.find((command) => command.type === 'prompt'))
      .toMatchObject({ prompt: '$deploy production' });
    expect(session.messages[0]).toMatchObject({ content: '$deploy production', display_content: '/deploy production' });
  });

  test('keeps expanded command content separate from display text when steering', async () => {
    const f = fixture();
    f.queryClient.setQueryData(f.key, f.history);
    await f.runtime.attachSession(f.history);
    await f.runtime.steerPrompt(f.history, '/review changes', [], 'Review changes carefully');
    expect(f.commands.find((command) => command.type === 'steer'))
      .toEqual({ type: 'steer', prompt: 'Review changes carefully' });
    f.emit('steerAccepted', { message: 'Review changes carefully' });
    expect(f.current().messages.at(-1))
      .toMatchObject({ content: 'Review changes carefully', display_content: '/review changes' });
  });

  test('queues expanded command content without starting another provider turn', async () => {
    const f = fixture();
    f.queryClient.setQueryData(f.key, f.history);
    const session = await f.runtime.sendPrompt(f.history, '/review changes', [], 'Review changes carefully');
    expect(session.queued_messages?.at(-1))
      .toMatchObject({ content: 'Review changes carefully', display_content: '/review changes' });
    expect(f.commands.some((command) => command.type === 'prompt')).toBe(false);
  });

  test('sends a goal operation to a live runtime without adding a prompt', async () => {
    const f = fixture();
    f.queryClient.setQueryData(f.key, f.history);
    await f.runtime.sendGoalOperation(f.history, { kind: 'set', objective: null, status: 'paused', replace: false });
    expect(f.commands.at(-1)).toEqual({ type: 'goal', operation: { kind: 'set', objective: null, status: 'paused', replace: false } });
    expect(f.current().messages).toEqual(f.history.messages);
    expect(f.commands.some((command) => command.type === 'prompt')).toBe(false);
  });

  test('starts a provider for a fresh goal and applies its native updates', async () => {
    const f = fixture({ attached: false });
    const fresh = { ...f.history, messages: [], turns: [], transcript_blocks: [], status: 'idle' as const };
    f.queryClient.setQueryData(f.key, fresh);
    await f.runtime.sendGoalOperation(fresh, { kind: 'set', objective: 'Ship mobile', status: 'active', replace: false });
    expect(f.commands.find((command) => command.type === 'start'))
      .toMatchObject({ options: { cwd: '/repo', provider: 'codex' } });
    expect(f.commands.at(-1)).toEqual({ type: 'goal', operation: { kind: 'set', objective: 'Ship mobile', status: 'active', replace: false } });
    expect(f.commands.some((command) => command.type === 'prompt')).toBe(false);
    f.emit('goalUpdated', { objective: 'Ship mobile', status: 'active', tokensUsed: 0, timeUsedSeconds: 0 });
    expect(f.current().thread_goal?.objective).toBe('Ship mobile');
    expect(f.current().messages).toEqual([]);
  });

  test('preserves history across two desktop steers after attaching from a list placeholder', async () => {
    const f = fixture();
    const loading = f.queryClient.fetchQuery({
      queryKey: f.key,
      queryFn: () => hydrateSession(f.client, f.history.id),
    });
    const attaching = f.runtime.attachSession(f.skeleton);
    await Promise.resolve();
    const commandsBeforeHydration = f.commands.map((command) => command.type);
    f.hydration.resolve(f.history);
    await loading;
    await attaching;

    f.emit('textDelta', ' before steering');
    f.emit('availableCommands', []);
    // The runtime outlives the screen's query-cache entry.
    f.queryClient.removeQueries({ queryKey: f.key });
    f.emit('steerAccepted', { message: 'First desktop steer' });
    f.emit('steerAccepted', { message: 'Second desktop steer' });

    expect(f.current().messages.map((message) => message.content)).toEqual([
      'Previous request', 'Previous answer', 'Current request',
      'Streaming response before steering', 'First desktop steer', 'Second desktop steer',
    ]);
    expect(f.current().turns).toEqual(f.history.turns);
    expect(commandsBeforeHydration).toEqual(['hydrateSession']);
    expect(f.commands.filter((command) => command.type === 'hydrateSession')).toHaveLength(1);
  });

  test('keeps the latest streamed history when the query cache is evicted before two desktop steers', async () => {
    const f = fixture();
    f.queryClient.setQueryData(f.key, f.history);
    await f.runtime.attachSession(f.history);
    f.emit('textDelta', ' before steering');
    // An interactive event flushes pending streaming text immediately.
    f.emit('availableCommands', []);
    f.queryClient.removeQueries({ queryKey: f.key });
    f.emit('steerAccepted', { message: 'First desktop steer' });
    f.emit('steerAccepted', { message: 'Second desktop steer' });
    f.emit('textDelta', 'Continued response');
    f.emit('availableCommands', []);

    expect(f.current().messages.map((message) => message.content)).toEqual([
      'Previous request', 'Previous answer', 'Current request',
      'Streaming response before steering', 'First desktop steer', 'Second desktop steer',
      'Continued response',
    ]);
    expect(f.current().turns).toEqual(f.history.turns);
  });
});
