#!/usr/bin/env bun

import { $ } from "bun";
import { watch, type FSWatcher } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const isMacOS = process.platform === "darwin";
const appName = "Waku Debug";
const targetDir = resolve(root, process.env.CARGO_TARGET_DIR || "target");
const executableSuffix = process.platform === "win32" ? ".exe" : "";
const appPath = isMacOS
  ? join(targetDir, "debug/Waku Debug.app")
  : join(targetDir, `debug/waku${executableSuffix}`);
const daemonPath = join(targetDir, `debug/waku-debug-daemon${executableSuffix}`);
const watchedDirectories = ["src", "crates", "assets", "resources", "locales"];
const watchedFiles = ["Cargo.toml", "Cargo.lock", "build.rs"];
const rebuildDebounceMs = 1_000;
type BuildTarget = "app" | "daemon";

$.cwd(root);

let app: ReturnType<typeof Bun.spawn> | undefined;
let stopping = false;
let building = false;
let queuedBuild: BuildTarget | undefined;
let debouncedBuild: BuildTarget | undefined;
let appChangeRevision = 0;
let daemonChangeRevision = 0;
let rebuildTimer: ReturnType<typeof setTimeout> | undefined;
const watchers: FSWatcher[] = [];

async function build(target: BuildTarget): Promise<boolean> {
  if (target === "daemon") {
    return buildDaemon();
  }

  console.log(`[waku-dev] Building ${isMacOS ? "app bundle" : "app"}...`);
  if (!(await buildDaemon())) {
    console.error("[waku-dev] Daemon build failed; keeping the current app open.");
    return false;
  }
  const result = isMacOS
    ? await $`${join(root, "scripts/bundle.sh")} debug`.nothrow()
    : await $`cargo build --package waku --bin waku --bin waku_js_repl`.nothrow();
  if (result.exitCode !== 0) {
    console.error("[waku-dev] Build failed; keeping the current app open.");
    return false;
  }
  return true;
}

async function buildDaemon(): Promise<boolean> {
  console.log("[waku-dev] Building daemon...");
  const result =
    await $`cargo build --package waku-daemon --features dev-binary --bin waku-debug-daemon`.nothrow();
  if (result.exitCode !== 0) {
    console.error("[waku-dev] Daemon build failed; keeping the current daemon running.");
    return false;
  }
  return true;
}

async function stopApp(): Promise<void> {
  const waiter = app;
  app = undefined;
  if (isMacOS) {
    await $`pkill -TERM -x ${appName}`.quiet().nothrow();
  } else if (waiter?.exitCode === null) {
    waiter.kill("SIGTERM");
  }
  if (waiter?.exitCode === null) {
    await waiter.exited;
  }
}

function launchApp(): ReturnType<typeof Bun.spawn> {
  console.log(`[waku-dev] Launching ${appPath}`);
  const command = isMacOS ? ["open", "-n", "-W", appPath] : [appPath];
  const launchedApp = Bun.spawn(command, {
    cwd: root,
    env: { ...process.env, WAKU_DAEMON_PATH: daemonPath },
    stdout: "inherit",
    stderr: "inherit",
  });
  void launchedApp.exited.then((exitCode) => {
    if (stopping || app !== launchedApp) return;
    app = undefined;
    stopping = true;
    closeWatchers();
    clearRebuildTimer();
    console.log("[waku-dev] App exited; stopping the watcher.");
    process.exitCode = exitCode;
  });
  return launchedApp;
}

function clearRebuildTimer(): void {
  if (rebuildTimer === undefined) return;
  clearTimeout(rebuildTimer);
  rebuildTimer = undefined;
}

function closeWatchers(): void {
  for (const watcher of watchers.splice(0)) watcher.close();
}

function reportWatcherError(error: Error): void {
  console.error("[waku-dev] File watcher failed:", error);
  process.exitCode = 1;
  void cleanup();
}

function mergedTarget(
  current: BuildTarget | undefined,
  next: BuildTarget,
): BuildTarget {
  return current === "app" || next === "app" ? "app" : "daemon";
}

function targetForChange(directory: string, filename: string | Buffer | null): BuildTarget {
  if (directory !== "crates" || filename === null) return "app";
  const relativePath = filename.toString().replaceAll("\\", "/");
  if (
    relativePath.startsWith("waku-daemon/") ||
    relativePath.startsWith("waku-core/")
  ) {
    return "daemon";
  }
  return "app";
}

function scheduleBuild(target: BuildTarget): void {
  if (stopping) return;
  daemonChangeRevision += 1;
  if (target === "app") appChangeRevision += 1;
  debouncedBuild = mergedTarget(debouncedBuild, target);
  clearRebuildTimer();
  rebuildTimer = setTimeout(() => {
    rebuildTimer = undefined;
    if (debouncedBuild !== undefined) {
      queuedBuild = mergedTarget(queuedBuild, debouncedBuild);
      debouncedBuild = undefined;
    }
    void drainBuildQueue();
  }, rebuildDebounceMs);
}

function startWatchers(): void {
  for (const directory of watchedDirectories) {
    const watcher = watch(
      join(root, directory),
      { recursive: true },
      (_eventType, filename) => scheduleBuild(targetForChange(directory, filename)),
    );
    watcher.on("error", reportWatcherError);
    watchers.push(watcher);
  }

  const rootWatcher = watch(root, (_eventType, filename) => {
    if (filename && watchedFiles.includes(filename.toString())) scheduleBuild("app");
  });
  rootWatcher.on("error", reportWatcherError);
  watchers.push(rootWatcher);
}

async function drainBuildQueue(): Promise<void> {
  if (building || stopping) return;
  building = true;
  try {
    while (queuedBuild !== undefined && !stopping) {
      const target = queuedBuild;
      queuedBuild = undefined;
      const buildAppRevision = appChangeRevision;
      const buildDaemonRevision = daemonChangeRevision;
      if (!(await build(target)) || stopping) continue;

      if (target === "daemon") {
        if (daemonChangeRevision === buildDaemonRevision) {
          console.log(
            "[waku-dev] Daemon rebuilt; Waku will swap the process without relaunching.",
          );
        }
        continue;
      }

      // App changes make a bundle compiled from an older revision stale. A
      // daemon-only edit does not: launch the app, then let its supervisor pick
      // up the independently rebuilt daemon.
      if (appChangeRevision !== buildAppRevision) {
        console.log(
          "[waku-dev] More changes arrived during the build; waiting to rebuild.",
        );
        continue;
      }

      await stopApp();
      if (!stopping) app = launchApp();
    }
  } finally {
    building = false;
    if (queuedBuild !== undefined && !stopping) void drainBuildQueue();
  }
}

async function cleanup(): Promise<void> {
  if (stopping) return;
  stopping = true;
  console.log("[waku-dev] Stopping watcher and app...");
  closeWatchers();
  clearRebuildTimer();
  await stopApp();
}

process.on("SIGINT", () => void cleanup());
process.on("SIGTERM", () => void cleanup());

startWatchers();
building = true;
const initialAppRevision = appChangeRevision;
const initialBuildSucceeded = await build("app");
building = false;
if (!initialBuildSucceeded) {
  closeWatchers();
  process.exit(1);
}

if (appChangeRevision === initialAppRevision) {
  await stopApp();
  app = launchApp();
} else {
  console.log(
    "[waku-dev] Changes arrived during the initial build; waiting to rebuild.",
  );
  if (queuedBuild !== undefined) void drainBuildQueue();
}

console.log(
  "[waku-dev] Watching for source changes. Daemon-only edits hot-reload without relaunching Waku.",
);
