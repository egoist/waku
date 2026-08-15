#!/usr/bin/env bun

import { $ } from "bun";
import { watch, type FSWatcher } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const isMacOS = process.platform === "darwin";
const appName = "Waku Debug";
const targetDir = resolve(root, process.env.CARGO_TARGET_DIR || "target");
const appPath = isMacOS
  ? join(targetDir, "debug/Waku Debug.app")
  : join(targetDir, "debug/waku");
const watchedDirectories = ["src", "assets", "resources", "locales"];
const watchedFiles = ["Cargo.toml", "Cargo.lock", "build.rs"];
const rebuildDebounceMs = 1_000;

$.cwd(root);

let app: ReturnType<typeof Bun.spawn> | undefined;
let stopping = false;
let building = false;
let buildQueued = false;
let changeRevision = 0;
let rebuildTimer: ReturnType<typeof setTimeout> | undefined;
const watchers: FSWatcher[] = [];

async function build(): Promise<boolean> {
  console.log(`[waku-dev] Building ${isMacOS ? "app bundle" : "app"}...`);
  const result = isMacOS
    ? await $`${join(root, "scripts/bundle.sh")} debug`.nothrow()
    : await $`cargo build --bin waku --bin waku_js_repl`.nothrow();
  if (result.exitCode !== 0) {
    console.error("[waku-dev] Build failed; keeping the current app open.");
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

function scheduleBuild(): void {
  if (stopping) return;
  changeRevision += 1;
  // A new event invalidates a queued build whose quiet period had elapsed
  // while another build was still running.
  buildQueued = false;
  clearRebuildTimer();
  rebuildTimer = setTimeout(() => {
    rebuildTimer = undefined;
    buildQueued = true;
    void drainBuildQueue();
  }, rebuildDebounceMs);
}

function startWatchers(): void {
  for (const directory of watchedDirectories) {
    const watcher = watch(
      join(root, directory),
      { recursive: true },
      scheduleBuild,
    );
    watcher.on("error", reportWatcherError);
    watchers.push(watcher);
  }

  const rootWatcher = watch(root, (_eventType, filename) => {
    if (filename && watchedFiles.includes(filename.toString())) scheduleBuild();
  });
  rootWatcher.on("error", reportWatcherError);
  watchers.push(rootWatcher);
}

async function drainBuildQueue(): Promise<void> {
  if (building || stopping) return;
  building = true;
  try {
    while (buildQueued && !stopping) {
      buildQueued = false;
      const buildRevision = changeRevision;
      if (!(await build()) || stopping) continue;

      // Never relaunch a bundle that became stale while it was compiling.
      // The debounce timer (or queued flag) will produce one follow-up build
      // after the current burst of writes settles.
      if (
        changeRevision !== buildRevision ||
        rebuildTimer !== undefined ||
        buildQueued
      ) {
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
    if (buildQueued && !stopping) void drainBuildQueue();
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
const initialRevision = changeRevision;
const initialBuildSucceeded = await build();
building = false;
if (!initialBuildSucceeded) {
  closeWatchers();
  process.exit(1);
}

if (
  changeRevision === initialRevision &&
  rebuildTimer === undefined &&
  !buildQueued
) {
  await stopApp();
  app = launchApp();
} else {
  console.log(
    "[waku-dev] Changes arrived during the initial build; waiting to rebuild.",
  );
  if (buildQueued) void drainBuildQueue();
}

console.log(
  "[waku-dev] Watching for source changes. Quit the app or press Ctrl-C to stop.",
);
