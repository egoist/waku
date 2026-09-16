export interface DaemonProfile {
  id: string;
  name: string;
  address: string;
  createdAt: number;
  updatedAt: number;
  lastConnectedAt: number | null;
}

export interface DaemonProfileInput {
  name: string;
  address: string;
  token?: string;
}

export function normalizeDaemonAddress(value: string): string {
  let address = value.trim();
  if (!address) throw new Error('Enter the daemon address');

  if (!/^[a-z][a-z\d+.-]*:\/\//i.test(address)) address = `ws://${address}`;
  if (/^http:\/\//i.test(address)) address = `ws://${address.slice(7)}`;
  if (/^https:\/\//i.test(address)) address = `wss://${address.slice(8)}`;

  let url: URL;
  try {
    url = new URL(address);
  } catch {
    throw new Error('Enter a valid ws:// or wss:// daemon address');
  }
  if (url.protocol !== 'ws:' && url.protocol !== 'wss:') {
    throw new Error('The daemon address must use ws:// or wss://');
  }
  if (!url.hostname || url.username || url.password) {
    throw new Error('The daemon address must contain a host and no credentials');
  }

  url.pathname = '';
  url.search = '';
  url.hash = '';
  return url.toString().replace(/\/$/, '');
}

export function normalizeDaemonProfile(
  input: DaemonProfileInput,
  existing: DaemonProfile | undefined,
  id: string,
  now = Date.now(),
): DaemonProfile {
  const address = normalizeDaemonAddress(input.address);
  const name = input.name.trim() || displayHost(address);
  return {
    id,
    name,
    address,
    createdAt: existing?.createdAt ?? now,
    updatedAt: now,
    lastConnectedAt: existing?.lastConnectedAt ?? null,
  };
}

export function displayHost(address: string): string {
  try {
    const url = new URL(normalizeDaemonAddress(address));
    return url.port ? `${url.hostname}:${url.port}` : url.hostname;
  } catch {
    return address;
  }
}

export function profileInitials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  if (!words.length) return 'W';
  if (words.length === 1) return [...words[0]!].slice(0, 2).join('').toUpperCase();
  return `${[...words[0]!][0] ?? ''}${[...words.at(-1)!][0] ?? ''}`.toUpperCase();
}

export function isPrivateDaemonAddress(address: string): boolean {
  try {
    const hostname = new URL(normalizeDaemonAddress(address)).hostname
      .toLowerCase()
      .replace(/^\[|\]$/g, '');
    if (hostname.includes(':')) {
      return hostname === '::1' || hostname.startsWith('fc') || hostname.startsWith('fd') ||
        hostname.startsWith('fe80:');
    }
    if (!hostname.includes('.')) return true;
    if (hostname === 'localhost' || hostname.endsWith('.local') || hostname.endsWith('.internal')) {
      return true;
    }
    if (
      /^127\./.test(hostname) || /^10\./.test(hostname) || /^192\.168\./.test(hostname) ||
      /^169\.254\./.test(hostname)
    ) {
      return true;
    }
    const match = hostname.match(/^172\.(\d{1,3})\./);
    if (match && Number(match[1]) >= 16 && Number(match[1]) <= 31) return true;
    if (/^(?:100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.)/.test(hostname)) return true;
    return false;
  } catch {
    return false;
  }
}

export function parseDaemonProfiles(value: unknown): DaemonProfile[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  const profiles: DaemonProfile[] = [];
  for (const item of value) {
    if (!item || typeof item !== 'object') continue;
    const record = item as Record<string, unknown>;
    if (
      typeof record.id !== 'string' || !record.id || seen.has(record.id) ||
      typeof record.name !== 'string' || typeof record.address !== 'string' ||
      typeof record.createdAt !== 'number' || !Number.isFinite(record.createdAt) ||
      typeof record.updatedAt !== 'number' || !Number.isFinite(record.updatedAt) ||
      (record.lastConnectedAt !== null && (
        typeof record.lastConnectedAt !== 'number' || !Number.isFinite(record.lastConnectedAt)
      ))
    ) {
      continue;
    }
    try {
      profiles.push({
        id: record.id,
        name: record.name.trim() || displayHost(record.address),
        address: normalizeDaemonAddress(record.address),
        createdAt: record.createdAt,
        updatedAt: record.updatedAt,
        lastConnectedAt: record.lastConnectedAt as number | null,
      });
      seen.add(record.id);
    } catch {
      // A malformed profile must not prevent other saved daemons from loading.
    }
  }
  return profiles;
}
