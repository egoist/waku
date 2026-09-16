import { queryOptions } from '@tanstack/react-query';
import {
  MAX_WIRE_MESSAGE_BYTES,
  type MessageAttachment,
  type WakuClient,
} from '@waku/client';

/** Leave enough JSON/base64 headroom for the websocket envelope, while also
 * respecting the daemon's 32 MiB per-attachment limit. */
export const MAX_ATTACHMENT_BYTES = Math.min(
  32 * 1024 * 1024,
  Math.floor((MAX_WIRE_MESSAGE_BYTES * 3) / 4) - 1024 * 1024,
);

export interface LocalAttachmentFile {
  uri: string;
  name: string;
  mimeType?: string | null;
  size?: number | null;
  /** Picker-provided data avoids reading the URI again when available. */
  base64?: string | null;
}

/** Shared by staged and sent attachments. Daemon paths belong to the host,
 * so even a phone on the same network must load bytes through its client. */
export async function readAttachmentImage(
  client: WakuClient,
  attachment: MessageAttachment,
): Promise<string> {
  const reference = attachment.blob_reference;
  if (!reference) throw new Error('This attachment has no daemon reference');
  const command = reference.startsWith('waku-blob:')
    ? ({ type: 'readBlob', reference } as const)
    : ({ type: 'readAttachment', reference, path: attachment.path } as const);
  const response = await client.request(command);
  if (response.type !== 'blobData') {
    throw new Error(`Expected blobData, received ${response.type}`);
  }
  return `data:${imageMimeType(attachment.name)};base64,${response.bytes}`;
}

export function attachmentImageQuery(
  client: WakuClient | null,
  profile: { id: string; address: string } | null,
  attachment: MessageAttachment,
  connected: boolean,
) {
  return queryOptions({
    queryKey: [
      'daemon', profile?.id ?? 'disconnected', 'attachment-image',
      profile?.address, attachment.blob_reference, attachment.path, attachment.name,
    ] as const,
    queryFn: () => {
      if (!client) throw new Error('Waku daemon is disconnected');
      return readAttachmentImage(client, attachment);
    },
    enabled: connected && Boolean(
      client && profile && attachment.is_image && !attachment.is_dir && attachment.blob_reference,
    ),
    // Stored attachments are immutable. Reuse the composer preview after send
    // and across row remounts, but release unused base64 data after a minute.
    staleTime: Infinity,
    gcTime: 60_000,
    retry: false,
  });
}

export async function importLocalAttachment(
  client: WakuClient,
  local: LocalAttachmentFile,
): Promise<MessageAttachment> {
  if (local.size != null && local.size > MAX_ATTACHMENT_BYTES) {
    throw attachmentTooLarge(local.name);
  }

  const encoded = local.base64 ?? await readBase64(local.uri);
  const dataBase64 = encoded.includes(',') ? encoded.slice(encoded.indexOf(',') + 1) : encoded;
  if (base64ByteLength(dataBase64) > MAX_ATTACHMENT_BYTES) {
    throw attachmentTooLarge(local.name);
  }

  const response = await client.request({
    type: 'importAttachment',
    name: local.name,
    upload: { kind: 'file', data_base64: dataBase64 },
  });
  if (response.type !== 'attachmentStored') {
    throw new Error(`Expected attachmentStored, received ${response.type}`);
  }

  return {
    path: response.attachment.path,
    mention: response.attachment.path,
    name: response.attachment.name,
    is_dir: response.attachment.isDir,
    is_image: local.mimeType?.startsWith('image/') === true || isImageName(local.name),
    blob_reference: response.attachment.reference,
  };
}

export function localFileName(uri: string, fallback: string): string {
  const segment = uri.split('/').at(-1)?.split(/[?#]/u)[0];
  if (!segment) return fallback;
  try {
    return decodeURIComponent(segment) || fallback;
  } catch {
    return segment;
  }
}

async function readBase64(uri: string): Promise<string> {
  // Kept behind the async boundary so Bun's pure projection tests do not load
  // an Expo native module. Metro still bundles the module for device builds.
  const { File } = await import('expo-file-system');
  return new File(uri).base64();
}

function base64ByteLength(value: string): number {
  const normalized = value.replace(/\s/gu, '');
  if (!normalized) return 0;
  const padding = normalized.endsWith('==') ? 2 : normalized.endsWith('=') ? 1 : 0;
  return Math.floor((normalized.length * 3) / 4) - padding;
}

function isImageName(name: string): boolean {
  return imageMimeType(name) !== 'application/octet-stream';
}

function imageMimeType(name: string): string {
  const types: Record<string, string> = {
    avif: 'image/avif',
    gif: 'image/gif',
    heic: 'image/heic',
    jpeg: 'image/jpeg',
    jpg: 'image/jpeg',
    png: 'image/png',
    svg: 'image/svg+xml',
    webp: 'image/webp',
  };
  const extension = name.split('.').at(-1)?.toLowerCase() ?? '';
  return Object.hasOwn(types, extension) ? types[extension]! : 'application/octet-stream';
}

function attachmentTooLarge(name: string): Error {
  return new Error(`${name} is too large to attach (32 MB maximum)`);
}
