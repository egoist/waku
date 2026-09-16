import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryObserver } from '@tanstack/react-query';
import type { Command, MessageAttachment, WakuClient } from '@waku/client';

import {
  attachmentImageQuery,
  importLocalAttachment,
  localFileName,
  MAX_ATTACHMENT_BYTES,
  readAttachmentImage,
} from './attachments';

const imageAttachment: MessageAttachment = {
  path: '/daemon/attachments/photo.png',
  mention: '/daemon/attachments/photo.png',
  name: 'photo.png',
  is_dir: false,
  is_image: true,
  blob_reference: 'waku-attachment:photo',
};

describe('mobile attachments', () => {
  test('imports local data through the daemon and retains display metadata', async () => {
    const commands: unknown[] = [];
    const client = {
      request: async (command: unknown) => {
        commands.push(command);
        return {
          type: 'attachmentStored',
          attachment: {
            reference: 'waku-attachment:file',
            path: '/daemon/blobs/photo.png',
            name: 'photo.png',
            isDir: false,
          },
        };
      },
    } as unknown as WakuClient;

    const attachment = await importLocalAttachment(client, {
      uri: 'file:///photo.png',
      name: 'photo.png',
      mimeType: 'image/png',
      size: 3,
      base64: 'data:image/png;base64,YWJj',
    });

    expect(commands).toEqual([{
      type: 'importAttachment',
      name: 'photo.png',
      upload: { kind: 'file', data_base64: 'YWJj' },
    }]);
    expect(attachment).toEqual({
      path: '/daemon/blobs/photo.png',
      mention: '/daemon/blobs/photo.png',
      name: 'photo.png',
      is_dir: false,
      is_image: true,
      blob_reference: 'waku-attachment:file',
    });
  });

  test('rejects an oversized file before reading or uploading it', async () => {
    const client = { request: () => Promise.reject(new Error('should not upload')) } as unknown as WakuClient;
    await expect(importLocalAttachment(client, {
      uri: 'file:///large.zip',
      name: 'large.zip',
      size: MAX_ATTACHMENT_BYTES + 1,
    })).rejects.toThrow('32 MB maximum');
  });

  test('derives a decoded name from a picker URI', () => {
    expect(localFileName('file:///tmp/Camera%20Photo.jpg?edited=1', 'photo.jpg'))
      .toBe('Camera Photo.jpg');
  });
});

describe('mobile attachment previews', () => {
  test('loads an attachment from the daemon instead of treating its host path as a local URI', async () => {
    const commands: Command[] = [];
    const client = {
      request: async (command: Command) => {
        commands.push(command);
        return { type: 'blobData', bytes: 'aW1hZ2U=' };
      },
    } as unknown as WakuClient;

    expect(await readAttachmentImage(client, imageAttachment)).toBe('data:image/png;base64,aW1hZ2U=');
    expect(commands).toEqual([{
      type: 'readAttachment',
      reference: 'waku-attachment:photo',
      path: '/daemon/attachments/photo.png',
    }]);
  });

  test('also reads the legacy image blobs used by desktop attachments', async () => {
    const commands: Command[] = [];
    const client = {
      request: async (command: Command) => {
        commands.push(command);
        return { type: 'blobData', bytes: 'aW1hZ2U=' };
      },
    } as unknown as WakuClient;

    const source = await readAttachmentImage(client, {
      ...imageAttachment,
      name: 'Camera.JPG',
      blob_reference: 'waku-blob:photo',
    });
    expect(source).toBe('data:image/jpeg;base64,aW1hZ2U=');
    expect(commands).toEqual([{ type: 'readBlob', reference: 'waku-blob:photo' }]);
  });

  test('preserves SVG MIME types for the native SVG renderer', async () => {
    const client = {
      request: async () => ({ type: 'blobData', bytes: 'PHN2Zy8+' }),
    } as unknown as WakuClient;
    expect(await readAttachmentImage(client, { ...imageAttachment, name: 'logo.svg' }))
      .toBe('data:image/svg+xml;base64,PHN2Zy8+');
  });

  test('keeps missing references and unexpected responses out of image sources', async () => {
    let requests = 0;
    const client = {
      request: async () => { requests++; return { type: 'ack' }; },
    } as unknown as WakuClient;
    await expect(readAttachmentImage(client, { ...imageAttachment, blob_reference: null }))
      .rejects.toThrow('no daemon reference');
    expect(requests).toBe(0);
    await expect(readAttachmentImage(client, imageAttachment)).rejects.toThrow('Expected blobData');
  });

  test('shares in-flight and cached image data between composer, sent message, and remounted rows', async () => {
    let requests = 0;
    const client = {
      request: async () => { requests++; return { type: 'blobData', bytes: 'aW1hZ2U=' }; },
    } as unknown as WakuClient;
    const cache = new QueryClient();
    const profile = { id: 'desktop', address: 'ws://desktop:4096' };
    const options = attachmentImageQuery(client, profile, imageAttachment, true);
    try {
      const [composer, sent] = await Promise.all([
        cache.fetchQuery(options),
        cache.fetchQuery(attachmentImageQuery(client, profile, { ...imageAttachment }, true)),
      ]);
      const remounted = await cache.fetchQuery(options);
      expect(composer).toBe('data:image/png;base64,aW1hZ2U=');
      expect(sent).toBe(composer);
      expect(remounted).toBe(composer);
      expect(requests).toBe(1);
    } finally {
      cache.clear();
    }
  });

  test('loads after reconnecting and never fetches file, folder, or missing-reference thumbnails', async () => {
    let requests = 0;
    const client = {
      request: async () => { requests++; return { type: 'blobData', bytes: 'aW1hZ2U=' }; },
    } as unknown as WakuClient;
    const cache = new QueryClient();
    const profile = { id: 'desktop', address: 'ws://desktop:4096' };
    const observer = new QueryObserver(cache, attachmentImageQuery(client, profile, imageAttachment, false));
    let loaded!: () => void;
    const finished = new Promise<void>((resolve) => { loaded = resolve; });
    const unsubscribe = observer.subscribe((result) => { if (result.isSuccess) loaded(); });
    try {
      expect(requests).toBe(0);
      for (const attachment of [
        { ...imageAttachment, is_image: false },
        { ...imageAttachment, is_dir: true },
        { ...imageAttachment, blob_reference: null },
      ]) {
        observer.setOptions(attachmentImageQuery(client, profile, attachment, true));
        expect(requests).toBe(0);
      }
      observer.setOptions(attachmentImageQuery(client, profile, imageAttachment, true));
      await finished;
      expect(requests).toBe(1);
      expect(observer.getCurrentResult().data).toBe('data:image/png;base64,aW1hZ2U=');
    } finally {
      unsubscribe();
      cache.clear();
    }
  });

  test('a late response cannot replace previews from another daemon or an edited daemon address', async () => {
    let resolveOld!: (response: unknown) => void;
    const oldClient = {
      request: () => new Promise((resolve) => { resolveOld = resolve; }),
    } as unknown as WakuClient;
    const newClient = {
      request: async () => ({ type: 'blobData', bytes: 'bmV3' }),
    } as unknown as WakuClient;
    const cache = new QueryClient();
    const previous = attachmentImageQuery(oldClient, { id: 'desktop', address: 'ws://old' }, imageAttachment, true);
    const edited = attachmentImageQuery(newClient, { id: 'desktop', address: 'ws://new' }, imageAttachment, true);
    const other = attachmentImageQuery(newClient, { id: 'laptop', address: 'ws://new' }, imageAttachment, true);
    try {
      const pending = cache.fetchQuery(previous);
      await cache.fetchQuery(edited);
      const otherData = cache.getQueryData(other.queryKey);
      expect(otherData).toBeUndefined();
      resolveOld({ type: 'blobData', bytes: 'b2xk' });
      await pending;
      expect(cache.getQueryData<string>(previous.queryKey)).toBe('data:image/png;base64,b2xk');
      expect(cache.getQueryData<string>(edited.queryKey)).toBe('data:image/png;base64,bmV3');
    } finally {
      cache.clear();
    }
  });
});
