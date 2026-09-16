import { useQueryClient } from "@tanstack/react-query";
import { WakuClient, type WebSocketLike } from "@waku/client";
import * as Crypto from "expo-crypto";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { AppState, Platform, type AppStateStatus } from "react-native";

import { daemonKeys } from "./daemon-api";
import { hydratePersistentStorage } from "./composer-preferences-store";
import { DaemonLink, type DaemonOutage } from "./daemon-link";
import {
  normalizeDaemonProfile,
  isPrivateDaemonAddress,
  type DaemonProfile,
  type DaemonProfileInput,
} from "./daemon-profile";
import {
  deleteDaemonToken,
  readActiveDaemonId,
  readDaemonProfiles,
  readDaemonToken,
  writeActiveDaemonId,
  writeDaemonProfiles,
  writeDaemonToken,
} from "./daemon-storage";

export type { DaemonOutage } from "./daemon-link";

/**
 * - `booting`: saved daemons are still loading.
 * - `disconnected`: no daemon is selected.
 * - `connecting`: the selected daemon's first attempt is in flight.
 * - `connected`: live.
 * - `reconnecting`: the link is down and retrying on its own; `outage`
 *   reports its progress.
 * - `error`: the link is down and waiting on the user; `error` says why.
 */
export type ConnectionPhase =
  | "booting"
  | "disconnected"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "error";

interface ConnectionStatus {
  phase: ConnectionPhase;
  error: string | null;
  outage: DaemonOutage | null;
  /** Successful connections to the selected daemon. Past the first, screens
   * refresh what the daemon may have changed while the link was down. */
  connections: number;
}

interface DaemonContextValue extends ConnectionStatus {
  profiles: DaemonProfile[];
  activeProfile: DaemonProfile | null;
  client: WakuClient | null;
  saveProfile: (
    input: DaemonProfileInput,
    id?: string,
  ) => Promise<{
    profile: DaemonProfile;
    connected: boolean;
  }>;
  selectProfile: (id: string) => Promise<boolean>;
  removeProfile: (id: string) => Promise<void>;
  /** Tries now: during an outage the wait is skipped and the backoff
   * restarts; after a hard failure the daemon is opened afresh with the
   * token stored now. */
  reconnect: () => Promise<boolean>;
  disconnect: () => void;
}

const DaemonContext = createContext<DaemonContextValue | null>(null);

const IDLE: ConnectionStatus = {
  phase: "disconnected",
  error: null,
  outage: null,
  connections: 0,
};

function createNativeDaemonSocket(url: string): WebSocketLike {
  // React Native adds an Origin header to native sockets. This marker lets the
  // daemon distinguish them from browser WebSockets, whose API cannot add
  // custom handshake headers. The DOM constructor type omits RN's third arg.
  const NativeWebSocket = WebSocket as unknown as {
    new (
      url: string,
      protocols: string | string[] | null,
      options: { headers: Record<string, string> },
    ): WebSocketLike;
  };
  return new NativeWebSocket(url, null, {
    headers: { "X-Waku-Client": "native" },
  });
}

export function DaemonProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [profiles, setProfiles] = useState<DaemonProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  const [client, setClient] = useState<WakuClient | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>({
    ...IDLE,
    phase: "booting",
  });
  const profilesRef = useRef<DaemonProfile[]>([]);
  const activeProfileIdRef = useRef<string | null>(null);
  const linkRef = useRef<DaemonLink | null>(null);
  const generation = useRef(0);
  const bootstrapped = useRef(false);
  const unsubscribeLink = useRef<(() => void) | null>(null);
  const unsubscribeTaskState = useRef<(() => void) | null>(null);

  const commitProfiles = useCallback(async (next: DaemonProfile[]) => {
    profilesRef.current = next;
    setProfiles(next);
    await writeDaemonProfiles(next);
  }, []);

  const closeCurrentLink = useCallback((updateReactState = true) => {
    unsubscribeLink.current?.();
    unsubscribeLink.current = null;
    unsubscribeTaskState.current?.();
    unsubscribeTaskState.current = null;
    const current = linkRef.current;
    linkRef.current = null;
    if (updateReactState) setClient(null);
    current?.close();
  }, []);

  /** Recency and cache upkeep for every (re)connection. */
  const noteConnected = useCallback(
    (profileId: string, connections: number) => {
      const connectedAt = Date.now();
      const updated = profilesRef.current.map((item) =>
        item.id === profileId ? { ...item, lastConnectedAt: connectedAt } : item,
      );
      profilesRef.current = updated;
      setProfiles(updated);
      void writeDaemonProfiles(updated).catch(() => {
        // A recency metadata write must not turn a live connection into an error state.
      });
      if (connections > 1) {
        // Task-state and session change notifications sent while the link
        // was down are gone, so refetch what screens are showing. Followed
        // runtimes replay their missed events on their own.
        void queryClient.invalidateQueries({
          queryKey: ["daemon", profileId],
          predicate: (query) =>
            query.queryKey[2] === "task-state" || query.queryKey[2] === "session",
        });
      }
    },
    [queryClient],
  );

  const activate = useCallback(
    async (
      profileId: string,
      candidates = profilesRef.current,
    ): Promise<boolean> => {
      const profile = candidates.find((item) => item.id === profileId);
      if (!profile) return false;

      const attempt = ++generation.current;
      closeCurrentLink();
      activeProfileIdRef.current = profile.id;
      setActiveProfileId(profile.id);
      setStatus({ ...IDLE, phase: "connecting" });

      try {
        await writeActiveDaemonId(profile.id);
      } catch (cause) {
        if (generation.current !== attempt) return false;
        setStatus({
          ...IDLE,
          phase: "error",
          error: errorMessage(cause, "Couldn’t remember the selected daemon"),
        });
        return false;
      }

      let token: string | null;
      try {
        token = await readDaemonToken(profile.id);
      } catch (cause) {
        if (generation.current !== attempt) return false;
        setStatus({
          ...IDLE,
          phase: "error",
          error: errorMessage(cause, "Couldn’t read this daemon token"),
        });
        return false;
      }
      if (generation.current !== attempt) return false;
      if (!token) {
        setStatus({
          ...IDLE,
          phase: "error",
          error:
            "This daemon token is missing. Edit the connection to add it again.",
        });
        return false;
      }

      const next = new WakuClient({
        address: profile.address,
        token,
        randomUUID: Crypto.randomUUID,
        webSocketFactory:
          Platform.OS === "web" ? undefined : createNativeDaemonSocket,
      });
      const link = new DaemonLink({
        client: next,
        active: inForeground(AppState.currentState),
      });
      linkRef.current = link;
      setClient(next);
      let seenConnections = 0;
      unsubscribeLink.current = link.subscribe((snapshot) => {
        if (
          generation.current !== attempt ||
          linkRef.current !== link ||
          snapshot.phase === "closed"
        ) {
          return;
        }
        setStatus({
          phase: snapshot.phase,
          error: snapshot.error,
          outage: snapshot.outage,
          connections: snapshot.connections,
        });
        if (snapshot.connections > seenConnections) {
          seenConnections = snapshot.connections;
          noteConnected(profile.id, snapshot.connections);
        }
      });
      unsubscribeTaskState.current = next.subscribeTaskState(() => {
        void queryClient.invalidateQueries({
          queryKey: daemonKeys.taskState(profile.id),
        });
      });

      const connected = await link.open();
      return (
        generation.current === attempt && linkRef.current === link && connected
      );
    },
    [closeCurrentLink, noteConnected, queryClient],
  );

  const reconnect = useCallback(async (): Promise<boolean> => {
    const id = activeProfileIdRef.current;
    if (!id) return false;
    const link = linkRef.current;
    if (!link) return activate(id);
    switch (link.state.phase) {
      case "connected":
        return true;
      case "connecting":
      case "reconnecting":
        return link.retryNow();
      default:
        // A hard failure gets a fresh client with whatever token is stored now.
        return activate(id);
    }
  }, [activate]);

  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    void hydratePersistentStorage();
    void (async () => {
      try {
        const [savedProfiles, savedActiveId] = await Promise.all([
          readDaemonProfiles(),
          readActiveDaemonId(),
        ]);
        profilesRef.current = savedProfiles;
        setProfiles(savedProfiles);
        if (!savedProfiles.length) {
          setStatus(IDLE);
          return;
        }
        const selected = savedProfiles.some((item) => item.id === savedActiveId)
          ? savedActiveId!
          : savedProfiles[0]!.id;
        await activate(selected, savedProfiles);
      } catch (cause) {
        setStatus({
          ...IDLE,
          phase: "error",
          error: errorMessage(cause, "Couldn’t load saved daemons"),
        });
      }
    })();
  }, [activate]);

  // The link retries and probes only while the app is in the foreground;
  // returning to it triggers an immediate retry or a liveness probe.
  useEffect(() => {
    const subscription = AppState.addEventListener("change", (state) => {
      linkRef.current?.setActive(inForeground(state));
    });
    return () => subscription.remove();
  }, []);

  useEffect(
    () => () => {
      ++generation.current;
      closeCurrentLink(false);
    },
    [closeCurrentLink],
  );

  const saveProfile = useCallback(
    async (
      input: DaemonProfileInput,
      id?: string,
    ): Promise<{ profile: DaemonProfile; connected: boolean }> => {
      const existing = id
        ? profilesRef.current.find((item) => item.id === id)
        : undefined;
      if (id && !existing)
        throw new Error("This saved daemon no longer exists");
      const profile = normalizeDaemonProfile(
        input,
        existing,
        id ?? Crypto.randomUUID(),
      );
      if (
        profile.address.startsWith("ws://") &&
        !isPrivateDaemonAddress(profile.address)
      ) {
        throw new Error(
          "Use wss:// for a daemon outside your local or private network",
        );
      }
      const duplicate = profilesRef.current.find(
        (item) => item.id !== profile.id && item.address === profile.address,
      );
      if (duplicate)
        throw new Error(`${duplicate.name} already uses this daemon address`);

      const token = input.token?.trim();
      if (!existing && !token) throw new Error("Enter the daemon token");
      if (token) await writeDaemonToken(profile.id, token);
      else if (!(await readDaemonToken(profile.id)))
        throw new Error("Enter the daemon token");

      const next = existing
        ? profilesRef.current.map((item) =>
            item.id === profile.id ? profile : item,
          )
        : [profile, ...profilesRef.current];
      await commitProfiles(next);
      const connected = await activate(profile.id, next);
      return { profile, connected };
    },
    [activate, commitProfiles],
  );

  const selectProfile = useCallback((id: string) => activate(id), [activate]);

  const removeProfile = useCallback(
    async (id: string) => {
      const current = profilesRef.current;
      if (!current.some((item) => item.id === id)) return;
      const next = current.filter((item) => item.id !== id);
      const token = await readDaemonToken(id);
      await deleteDaemonToken(id);
      try {
        await commitProfiles(next);
      } catch (cause) {
        if (token) await writeDaemonToken(id, token);
        throw cause;
      }
      queryClient.removeQueries({ queryKey: ["daemon", id] });
      if (activeProfileIdRef.current !== id) return;

      ++generation.current;
      closeCurrentLink();
      activeProfileIdRef.current = null;
      setActiveProfileId(null);
      await writeActiveDaemonId(null);
      setStatus(IDLE);
      if (next.length) await activate(next[0]!.id, next);
    },
    [activate, closeCurrentLink, commitProfiles, queryClient],
  );

  const disconnect = useCallback(() => {
    ++generation.current;
    closeCurrentLink();
    setStatus(IDLE);
  }, [closeCurrentLink]);

  const activeProfile =
    profiles.find((item) => item.id === activeProfileId) ?? null;
  return (
    <DaemonContext.Provider
      value={{
        profiles,
        activeProfile,
        client,
        phase: status.phase,
        error: status.error,
        outage: status.outage,
        connections: status.connections,
        saveProfile,
        selectProfile,
        removeProfile,
        reconnect,
        disconnect,
      }}
    >
      {children}
    </DaemonContext.Provider>
  );
}

export function useDaemon() {
  const context = useContext(DaemonContext);
  if (!context) throw new Error("useDaemon must be used inside DaemonProvider");
  return context;
}

/** Only a backgrounded (or transitioning) app pauses the link; an `unknown`
 * state at launch must not leave it waiting for a change that never comes. */
function inForeground(state: AppStateStatus): boolean {
  return state !== "background" && state !== "inactive";
}

function errorMessage(cause: unknown, fallback: string): string {
  if (cause instanceof Error && cause.message.trim()) return cause.message;
  return fallback;
}
