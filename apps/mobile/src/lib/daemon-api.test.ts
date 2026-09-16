import { describe, expect, test } from 'bun:test';
import type { WakuClient } from '@waku/client';

import { browseDaemonDirectory, createProject, daemonKeys, discoverComposerCommands, listComposerFiles, persistProject, providerSessionKey } from './daemon-api';

describe('mobile daemon API', () => {
  test('requests composer catalogs from the selected daemon workspace and provider override', async () => {
    const commands: unknown[] = [];
    const files = [{ path: 'src/', is_dir: true }];
    const client = {
      request: async (command: any) => {
        commands.push(command);
        return { type: 'workspace', result: command.operation.type === 'listProjectFiles'
          ? { type: 'projectFiles', entries: files }
          : { type: 'slashCommands', commands: [] } };
      },
    } as unknown as WakuClient;
    expect(await discoverComposerCommands(client, 'codex', '/worktree', '/opt/codex')).toEqual([]);
    expect(await listComposerFiles(client, '/worktree')).toEqual(files);
    expect(commands).toEqual([
      { type: 'workspace', operation: { type: 'discoverSlashCommands', provider: 'codex', project_root: '/worktree', binary_override: '/opt/codex' } },
      { type: 'workspace', operation: { type: 'listProjectFiles', root: '/worktree', cap: 50_000 } },
    ]);
  });

  test('isolates command and file caches across daemons, projects, providers, and binaries', () => {
    const keys = [
      daemonKeys.composerCommands('one', 'codex', '/repo', null),
      daemonKeys.composerCommands('two', 'codex', '/repo', null),
      daemonKeys.composerCommands('one', 'claude', '/repo', null),
      daemonKeys.composerCommands('one', 'codex', '/worktree', null),
      daemonKeys.composerCommands('one', 'codex', '/repo', '/other/codex'),
      daemonKeys.composerFiles('one', '/repo'),
      daemonKeys.composerFiles('two', '/repo'),
      daemonKeys.composerFiles('one', '/worktree'),
    ];
    expect(new Set(keys.map((key) => JSON.stringify(key))).size).toBe(keys.length);
  });

  test('rejects unexpected catalog replies instead of caching an empty success', async () => {
    const client = { request: async () => ({ type: 'ack' }) } as unknown as WakuClient;
    await expect(listComposerFiles(client, '/repo')).rejects.toThrow('Expected daemon response workspace');
    await expect(discoverComposerCommands(client, 'codex', '/repo', null)).rejects.toThrow('Expected daemon response workspace');
  });

  test('identifies provider history by its native session id', () => {
    expect(providerSessionKey({ provider: 'codex', threadId: 'thread' })).toBe('codex:thread');
    expect(providerSessionKey({ provider: 'claude', sessionId: 'session' })).toBe('claude:session');
  });
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
