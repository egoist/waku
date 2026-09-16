import AsyncStorage from '@react-native-async-storage/async-storage';
import * as SecureStore from 'expo-secure-store';

import { parseDaemonProfiles, type DaemonProfile } from './daemon-profile';

const PROFILES_KEY = 'waku.mobile.daemons.v1';
const ACTIVE_KEY = 'waku.mobile.active-daemon.v1';
const TOKEN_PREFIX = 'waku.mobile.daemon-token.v1.';

export async function readDaemonProfiles(): Promise<DaemonProfile[]> {
  const serialized = await AsyncStorage.getItem(PROFILES_KEY);
  if (!serialized) return [];
  try {
    return parseDaemonProfiles(JSON.parse(serialized));
  } catch {
    return [];
  }
}

export async function writeDaemonProfiles(profiles: DaemonProfile[]): Promise<void> {
  await AsyncStorage.setItem(PROFILES_KEY, JSON.stringify(profiles));
}

export async function readActiveDaemonId(): Promise<string | null> {
  return AsyncStorage.getItem(ACTIVE_KEY);
}

export async function writeActiveDaemonId(id: string | null): Promise<void> {
  if (id) await AsyncStorage.setItem(ACTIVE_KEY, id);
  else await AsyncStorage.removeItem(ACTIVE_KEY);
}

export async function readDaemonToken(id: string): Promise<string | null> {
  return SecureStore.getItemAsync(tokenKey(id));
}

export async function writeDaemonToken(id: string, token: string): Promise<void> {
  await SecureStore.setItemAsync(tokenKey(id), token, {
    keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
  });
}

export async function deleteDaemonToken(id: string): Promise<void> {
  await SecureStore.deleteItemAsync(tokenKey(id));
}

function tokenKey(id: string): string {
  return `${TOKEN_PREFIX}${id}`;
}
