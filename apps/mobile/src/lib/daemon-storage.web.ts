import { parseDaemonProfiles, type DaemonProfile } from './daemon-profile';

const PROFILES_KEY = 'waku.mobile.daemons.v1';
const ACTIVE_KEY = 'waku.mobile.active-daemon.v1';
const TOKEN_PREFIX = 'waku.mobile.daemon-token.v1.';

export async function readDaemonProfiles(): Promise<DaemonProfile[]> {
  const serialized = storage()?.getItem(PROFILES_KEY);
  if (!serialized) return [];
  try {
    return parseDaemonProfiles(JSON.parse(serialized));
  } catch {
    return [];
  }
}

export async function writeDaemonProfiles(profiles: DaemonProfile[]): Promise<void> {
  storage()?.setItem(PROFILES_KEY, JSON.stringify(profiles));
}

export async function readActiveDaemonId(): Promise<string | null> {
  return storage()?.getItem(ACTIVE_KEY) ?? null;
}

export async function writeActiveDaemonId(id: string | null): Promise<void> {
  if (id) storage()?.setItem(ACTIVE_KEY, id);
  else storage()?.removeItem(ACTIVE_KEY);
}

export async function readDaemonToken(id: string): Promise<string | null> {
  return storage()?.getItem(`${TOKEN_PREFIX}${id}`) ?? null;
}

export async function writeDaemonToken(id: string, token: string): Promise<void> {
  storage()?.setItem(`${TOKEN_PREFIX}${id}`, token);
}

export async function deleteDaemonToken(id: string): Promise<void> {
  storage()?.removeItem(`${TOKEN_PREFIX}${id}`);
}

function storage(): Storage | null {
  return typeof localStorage === 'undefined' ? null : localStorage;
}
