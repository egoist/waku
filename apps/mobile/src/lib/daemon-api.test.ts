import { describe, expect, test } from 'bun:test';
import type {
  ProviderResumeCursor,
  ProviderSessionHistory,
  ProviderSessionSummary,
  WakuClient,
} from '@waku/client';

import {
  browseDaemonDirectory,
  createProject,
  createResumedSession,
  listProviderSessions,
  loadProviderSessionHistory,
  persistProject,
  providerSessionNativeId,
  sameProviderSession,
} from './daemon-api';

describe('mobile daemon API', () => {
  test('browses a directory on the remote host', async () => {
    let command: unknown;
    const directory = {
      type: 'directory' as const,
      path: '/Users/me',
      parent: '/Users',
      home: '/Users/me',
      filesystem_root: '/',
      entries: [],
    };
    const client = {
      request: async (next: unknown) => {
        command = next;
        return { type: 'workspace', result: directory };
      },
    } as unknown as WakuClient;

    await expect(browseDaemonDirectory(client, null)).resolves.toEqual(directory);
    expect(command).toEqual({
      type: 'workspace',
      operation: { type: 'browseDirectory', path: null },
    });
  });

  test('normalizes absolute Unix and Windows project paths', () => {
    expect(createProject('/srv/waku/', 'one', 10)).toEqual({
      id: 'one',
      name: 'waku',
      path: '/srv/waku',
      created_at: 10,
    });
    expect(createProject('C:\\dev\\waku\\', 'two', 10).name).toBe('waku');
    expect(() => createProject('dev/waku', 'three')).toThrow('absolute path');
  });

  test('persists a project without replacing sessions', async () => {
    const commands: unknown[] = [];
    const client = {
      request: async (command: any) => {
        commands.push(command);
        if (command.type === 'loadTaskState') {
          return {
            type: 'taskState',
            revision: 2,
            projects: [],
            sessions: [{ id: 'live' }],
          };
        }
        return { type: 'taskStateSaved', revision: 3, sessions: [] };
      },
    } as unknown as WakuClient;
    const project = createProject('/srv/waku', 'project', 10);

    const saved = await persistProject(client, project);
    expect(saved.project).toEqual(project);
    expect(commands[1]).toEqual({
      type: 'saveTaskState',
      projects: [project],
      liveSessionIds: ['live'],
      sessions: [],
    });
  });
});

describe('provider session resume', () => {
  function cursor(provider: 'codex' | 'claude', id: string): ProviderResumeCursor {
    return (provider === 'codex'
      ? { provider: 'codex', threadId: id }
      : { provider: 'claude', sessionId: id }) as unknown as ProviderResumeCursor;
  }

  function summary(provider: 'codex' | 'claude', id: string): ProviderSessionSummary {
    return {
      title: 'Old task',
      cwd: '/srv/waku',
      created_at: 100,
      updated_at: 120,
      cursor: cursor(provider, id),
    } as unknown as ProviderSessionSummary;
  }

  test('lists and loads external sessions through the daemon', async () => {
    const listed = summary('claude', 'native-1');
    const history: ProviderSessionHistory = { messages: [], turns: [] };
    const client = {
      request: async (command: any) => {
        if (command.type === 'listProviderSessions') {
          return { type: 'providerSessions', sessions: [listed] };
        }
        return { type: 'providerSessionHistory', history };
      },
    } as unknown as WakuClient;

    await expect(listProviderSessions(client, 'claude', 250)).resolves.toEqual([listed]);
    await expect(loadProviderSessionHistory(client, listed)).resolves.toEqual(history);
  });

  test('matches sessions by their native provider id', () => {
    const left = cursor('codex', 'thread-9');
    const right = cursor('codex', 'thread-9');
    const other = cursor('codex', 'thread-8');
    expect(providerSessionNativeId(left)).toBe('thread-9');
    expect(sameProviderSession(left, right)).toBe(true);
    expect(sameProviderSession(left, other)).toBe(false);
    // A different provider reusing the id is a different session.
    expect(sameProviderSession(left, cursor('claude', 'thread-9'))).toBe(false);
  });

  test('copies the history into a resumed task', () => {
    const source = summary('codex', 'thread-1');
    const history: ProviderSessionHistory = {
      messages: [{
        id: 'm',
        turn_id: 't',
        role: 'user',
        content: 'hi',
        created_at: 1,
        streaming: false,
      }],
      turns: [{
        id: 't',
        turn_count: 1,
        status: 'completed',
        provider_turn_started: true,
        provider_resume_at: null,
        started_at: 1,
        completed_at: 2,
        checkpoint: null,
      }],
    } as unknown as ProviderSessionHistory;
    const resumed = createResumedSession('project', source, history, 'ask');

    expect(resumed.project_id).toBe('project');
    expect(resumed.provider).toBe('codex');
    expect(resumed.provider_cursor).toEqual(source.cursor);
    expect(resumed.runtime_mode).toBe('ask');
    expect(resumed.messages).toEqual(history.messages);
    expect(resumed.turns).toEqual(history.turns);
    expect(resumed.status).toBe('idle');
    expect(resumed.last_reply_at).not.toBeNull();
  });
});
