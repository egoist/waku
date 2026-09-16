import AsyncStorage from '@react-native-async-storage/async-storage';
import type { RuntimeMode } from '@waku/client';
import {
  readComposerPreferences,
  writeComposerPreferences,
  type ComposerPreferences,
} from '@waku/client/composer-preferences';

/** Mobile-only additions to the shared composer preferences: the New Task
 * page also restores access mode, workspace choice, and project. */
export interface NewTaskExtras {
  runtimeMode: RuntimeMode;
  isolated: boolean;
  projectId: string | null;
}

const EXTRAS_KEY = 'waku.mobile.new-task.v1';
const DEFAULT_EXTRAS: NewTaskExtras = {
  runtimeMode: 'fullAccess',
  isolated: false,
  projectId: null,
};

const memory = new Map<string, string>();
let hydrated: Promise<void> | null = null;
let hydratedDone = false;

const PERSISTED_KEYS = [
  'waku.composer-preferences.v1',
  'waku.provider-probes.v1',
  EXTRAS_KEY,
];

const memoryStorage: Pick<Storage, 'getItem' | 'setItem'> = {
  getItem: (key) => memory.get(key) ?? null,
  setItem: (key, value) => {
    memory.set(key, value);
    void AsyncStorage.setItem(key, value).catch(() => {});
  },
};

/** The shared preference/cache modules want a synchronous Storage; hydrate
 * the known keys once per app run and write through to AsyncStorage. */
export function hydratePersistentStorage(): Promise<void> {
  hydrated ??= (async () => {
    const entries = await AsyncStorage.multiGet(PERSISTED_KEYS);
    for (const [key, value] of entries) {
      if (value != null) memory.set(key, value);
    }
    hydratedDone = true;
  })();
  return hydrated;
}

/** Synchronous view for render-time reads (react-query initialData); null
 * until hydration completes, which the daemon bootstrap kicks off. */
export function persistentStorageSync(): Pick<Storage, 'getItem' | 'setItem'> | null {
  if (!hydratedDone) {
    void hydratePersistentStorage();
    return null;
  }
  return memoryStorage;
}

async function storage(): Promise<Pick<Storage, 'getItem' | 'setItem'>> {
  await hydratePersistentStorage();
  return memoryStorage;
}

export async function loadComposerPreferences(
  daemonAddress: string,
): Promise<ComposerPreferences> {
  return readComposerPreferences(await storage(), daemonAddress);
}

export async function saveComposerPreferences(
  daemonAddress: string,
  preferences: ComposerPreferences,
): Promise<void> {
  writeComposerPreferences(await storage(), daemonAddress, preferences);
}

export async function loadNewTaskExtras(daemonAddress: string): Promise<NewTaskExtras> {
  const store = await storage();
  try {
    const entries = JSON.parse(store.getItem(EXTRAS_KEY) ?? '{}') as Record<string, unknown>;
    const value = entries[daemonAddress];
    if (typeof value !== 'object' || value === null) return { ...DEFAULT_EXTRAS };
    const extras = value as Partial<NewTaskExtras>;
    return {
      runtimeMode: isRuntimeMode(extras.runtimeMode) ? extras.runtimeMode : 'fullAccess',
      isolated: extras.isolated === true,
      projectId: typeof extras.projectId === 'string' ? extras.projectId : null,
    };
  } catch {
    return { ...DEFAULT_EXTRAS };
  }
}

export async function saveNewTaskExtras(
  daemonAddress: string,
  extras: NewTaskExtras,
): Promise<void> {
  const store = await storage();
  let entries: Record<string, unknown> = {};
  try {
    const parsed = JSON.parse(store.getItem(EXTRAS_KEY) ?? '{}');
    if (typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed)) {
      entries = parsed as Record<string, unknown>;
    }
  } catch {
    // Malformed disposable state; replace it.
  }
  entries[daemonAddress] = extras;
  store.setItem(EXTRAS_KEY, JSON.stringify(entries));
}

function isRuntimeMode(value: unknown): value is RuntimeMode {
  return value === 'ask' || value === 'autoAcceptEdits' ||
    value === 'auto' || value === 'fullAccess';
}
