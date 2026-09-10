import { describe, expect, test } from 'bun:test';
import type { MessageAttachment, WakuClient } from '@waku/client';

import {
  imageMimeType,
  importLocalAttachment,
  isPreviewableImage,
  localFileName,
  MAX_ATTACHMENT_BYTES,
  readAttachmentImage,
} from './attachments';

function attachment(overrides: Partial<MessageAttachment> = {}): MessageAttachment {
  return {
    path: '/daemon/blobs/photo.png',
    mention: '/daemon/blobs/photo.png',
    name: 'photo.png',
    is_dir: false,
    is_image: true,
    blob_reference: 'waku-attachment:file',
    ...overrides,
  };
}

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

describe('attachment previews', () => {
  test('reads a stored attachment back as a data URI', async () => {
    const commands: unknown[] = [];
    const client = {
      request: async (command: unknown) => {
        commands.push(command);
        return { type: 'blobData', bytes: 'YWJj' };
      },
    } as unknown as WakuClient;

    await expect(readAttachmentImage(client, attachment())).resolves.toBe(
      'data:image/png;base64,YWJj',
    );
    expect(commands).toEqual([{
      type: 'readAttachment',
      reference: 'waku-attachment:file',
      path: '/daemon/blobs/photo.png',
    }]);
  });

  test('reads a Waku-owned blob without a path', async () => {
    const commands: unknown[] = [];
    const client = {
      request: async (command: unknown) => {
        commands.push(command);
        return { type: 'blobData', bytes: 'YWJj' };
      },
    } as unknown as WakuClient;

    await readAttachmentImage(client, attachment({ blob_reference: 'waku-blob:abc' }));
    expect(commands).toEqual([{ type: 'readBlob', reference: 'waku-blob:abc' }]);
  });

  test('refuses an attachment the daemon cannot look up', async () => {
    const client = {
      request: () => Promise.resolve({ type: 'ack' }),
    } as unknown as WakuClient;
    await expect(readAttachmentImage(client, attachment())).rejects.toThrow('Expected blobData');
    await expect(readAttachmentImage(client, attachment({ blob_reference: null }))).rejects.toThrow(
      'no daemon reference',
    );
  });

  test('maps image extensions to MIME types and falls back to png', () => {
    expect(imageMimeType('shot.PNG')).toBe('image/png');
    expect(imageMimeType('shot.jpg')).toBe('image/jpeg');
    expect(imageMimeType('shot.jpeg')).toBe('image/jpeg');
    expect(imageMimeType('clip.webp')).toBe('image/webp');
    expect(imageMimeType('clip.gif')).toBe('image/gif');
    expect(imageMimeType('archive')).toBe('image/png');
  });

  test('previews only images the decoder can rasterize', () => {
    expect(isPreviewableImage(attachment())).toBe(true);
    expect(isPreviewableImage(attachment({ is_image: false }))).toBe(false);
    expect(isPreviewableImage(attachment({ blob_reference: null }))).toBe(false);
    // RN cannot decode SVG, so it keeps the name chip instead of a blank tile.
    expect(isPreviewableImage(attachment({ name: 'logo.svg' }))).toBe(false);
  });
});
