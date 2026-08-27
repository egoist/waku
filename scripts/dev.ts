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
const daemonPath = join(
  targetDir,
  `debug/waku-debug-daemon${executableSuffix}`,
);
const watchedDirectories = ["src", "crates", "assets", "resources", "locales"];
const watchedFiles = ["Cargo.toml", "Cargo.lock", "build.rs"];
const rebuildDebounceMs = 1_000;
type BuildTarget = "app" | "daemon";
type HyprlandWorkspace = {
  id: number;
  name: string;
  selector: string;
};
type HyprlandContext = {
  workspace: HyprlandWorkspace;
  anchorSelector?: string;
};

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
const hyprlandRuleKeys = [
  "waku_dev_workspace_rule",
  "waku_dev_background_rule",
] as const;
const hyprlandSubscriptionKey = "waku_dev_window_open_subscription";
const hyprlandLaunchArmedKey = "waku_dev_launch_armed";
const hyprlandOwnerKey = "waku_dev_owner";
let hyprlandRulesInstalled = false;
let hyprlandWarningShown = false;

function luaString(value: string): string {
  const bytes = new TextEncoder().encode(value);
  const escaped = Array.from(
    bytes,
    (byte) => `\\${byte.toString().padStart(3, "0")}`,
  ).join("");
  return `"${escaped}"`;
}

async function activeHyprlandContext(): Promise<HyprlandContext | undefined> {
  if (
    process.platform !== "linux" ||
    process.env.HYPRLAND_INSTANCE_SIGNATURE === undefined
  ) {
    return undefined;
  }

  const [workspaceResult, windowResult] = await Promise.all([
    $`hyprctl -j activeworkspace`.quiet().nothrow(),
    $`hyprctl -j activewindow`.quiet().nothrow(),
  ]);
  if (workspaceResult.exitCode !== 0) return undefined;

  try {
    const workspace = JSON.parse(workspaceResult.stdout.toString()) as {
      id?: unknown;
      name?: unknown;
    };
    if (
      typeof workspace.id !== "number" ||
      !Number.isInteger(workspace.id) ||
      typeof workspace.name !== "string" ||
      workspace.name.length === 0
    ) {
      return undefined;
    }
    const context: HyprlandContext = {
      workspace: {
        id: workspace.id,
        name: workspace.name,
        selector:
          workspace.id > 0 ? workspace.id.toString() : `name:${workspace.name}`,
      },
    };

    if (windowResult.exitCode === 0) {
      try {
        const window = JSON.parse(windowResult.stdout.toString()) as {
          stableId?: unknown;
          workspace?: { id?: unknown; name?: unknown };
        };
        if (
          typeof window.stableId === "string" &&
          /^[0-9a-f]+$/i.test(window.stableId) &&
          window.workspace?.id === workspace.id &&
          window.workspace.name === workspace.name
        ) {
          context.anchorSelector = `stableid:${window.stableId.toLowerCase()}`;
        }
      } catch {
        // The workspace rule still works when there is no usable anchor.
      }
    }

    return context;
  } catch {
    return undefined;
  }
}

// Hyprland normally maps a new window onto whichever workspace is active and,
// in the scrolling layout, inserts it after the focused window. Remember the
// watcher terminal as well as its workspace so background rebuilds can retain
// both the destination and the neighboring column.
const hyprlandContext = await activeHyprlandContext();

async function prepareHyprlandLaunch(): Promise<void> {
  if (hyprlandContext === undefined) return;

  const { workspace: hyprlandWorkspace, anchorSelector } = hyprlandContext;
  const [workspaceRuleKey, backgroundRuleKey] = hyprlandRuleKeys;
  const isAnotherWorkspaceActive =
    hyprlandWorkspace.id > 0
      ? `active == nil or active.id ~= ${hyprlandWorkspace.id}`
      : `active == nil or active.name ~= ${luaString(hyprlandWorkspace.name)}`;
  const code = `
    local workspace_key = ${luaString(workspaceRuleKey)}
    local background_key = ${luaString(backgroundRuleKey)}
    local subscription_key = ${luaString(hyprlandSubscriptionKey)}
    local armed_key = ${luaString(hyprlandLaunchArmedKey)}
    local owner_key = ${luaString(hyprlandOwnerKey)}
    local owner = ${process.pid}

    if _G[owner_key] ~= owner then
      if _G[workspace_key] ~= nil then
        _G[workspace_key]:set_enabled(false)
      end
      if _G[background_key] ~= nil then
        _G[background_key]:set_enabled(false)
      end
      if _G[subscription_key] ~= nil then
        _G[subscription_key]:remove()
      end
      _G[workspace_key] = nil
      _G[background_key] = nil
      _G[subscription_key] = nil
      _G[owner_key] = owner
    end

    if _G[workspace_key] == nil then
      _G[workspace_key] = hl.window_rule({
        name = "waku-dev-workspace",
        match = { initial_class = "sh[.]waku[.]dev" },
        workspace = ${luaString(`${hyprlandWorkspace.selector} silent`)},
      })
    end
    if _G[background_key] == nil then
      _G[background_key] = hl.window_rule({
        name = "waku-dev-background",
        match = { initial_class = "sh[.]waku[.]dev" },
        no_initial_focus = true,
        suppress_event = "activate activatefocus",
      })
    end
    ${
      anchorSelector === undefined
        ? ""
        : `
    if _G[subscription_key] == nil then
      local anchor_selector = ${luaString(anchorSelector)}
      _G[subscription_key] = hl.on("window.open", function(window)
        if not _G[armed_key] or window.initial_class ~= "sh.waku.dev" then
          return
        end
        _G[armed_key] = false

        local anchor = hl.get_window(anchor_selector)
        if anchor == nil or anchor.workspace == nil or window.workspace ~= anchor.workspace then
          return
        end

        local anchor_layout = anchor.layout
        local window_layout = window.layout
        if anchor_layout == nil or window_layout == nil or
            anchor_layout.name ~= "scrolling" or window_layout.name ~= "scrolling" or
            anchor_layout.column == nil or window_layout.column == nil then
          return
        end

        local desired_index = anchor_layout.column.index + 1
        local current_index = window_layout.column.index
        if current_index <= desired_index or #window_layout.column.windows ~= 1 then
          return
        end

        -- Swapping with each preceding singleton column rotates Waku into the
        -- desired slot while preserving the order of all intervening columns.
        -- A stacked or custom-width column cannot be rotated through this API
        -- without changing its membership or sizing, so leave it untouched.
        local columns = {}
        for _, candidate in ipairs(hl.get_workspace_windows(anchor.workspace)) do
          local layout = candidate.layout
          local column = layout ~= nil and layout.name == "scrolling" and layout.column or nil
          if column ~= nil and column.index >= desired_index and column.index < current_index then
            if #column.windows ~= 1 or math.abs(column.width - window_layout.column.width) > 0.0001 then
              return
            end
            columns[column.index] = column.windows[1]
          end
        end
        for index = desired_index, current_index - 1 do
          if columns[index] == nil then
            return
          end
        end

        -- Hyprland's swap action warps the pointer to its source window. Hold
        -- mouse focus steady and restore the exact pointer position afterward.
        local cursor = hl.get_cursor_pos()
        local follow_mouse = hl.get_config("input.follow_mouse")
        if cursor == nil or type(follow_mouse) ~= "number" then
          return
        end

        hl.config({ input = { follow_mouse = 0 } })
        pcall(function()
          for index = current_index - 1, desired_index, -1 do
            hl.dispatch(hl.dsp.window.swap({ window = window, target = columns[index] }))
          end
        end)
        hl.dispatch(hl.dsp.cursor.move({ x = cursor.x, y = cursor.y }))
        hl.config({ input = { follow_mouse = follow_mouse } })
      end)
    end
    `
    }
    local active = hl.get_active_workspace()
    _G[background_key]:set_enabled(${isAnotherWorkspaceActive})
    _G[armed_key] = true
  `;
  const result = await $`hyprctl eval ${code}`.quiet().nothrow();
  if (result.exitCode !== 0) {
    if (!hyprlandWarningShown) {
      const detail =
        result.stderr.toString().trim() || result.stdout.toString().trim();
      console.warn(
        `[waku-dev] Could not pin Waku to its Hyprland workspace${detail ? `: ${detail}` : "."}`,
      );
      hyprlandWarningShown = true;
    }
    return;
  }

  if (!hyprlandRulesInstalled) {
    console.log(
      `[waku-dev] Keeping Waku beside the watcher on Hyprland workspace ${hyprlandWorkspace.name}.`,
    );
  }
  hyprlandRulesInstalled = true;
}

async function releaseHyprlandRules(): Promise<void> {
  if (!hyprlandRulesInstalled) return;
  hyprlandRulesInstalled = false;
  const code = `
    local owner_key = ${luaString(hyprlandOwnerKey)}
    if _G[owner_key] == ${process.pid} then
      for _, key in ipairs({ ${hyprlandRuleKeys.map(luaString).join(", ")} }) do
        if _G[key] ~= nil then
          _G[key]:set_enabled(false)
          _G[key] = nil
        end
      end
      local subscription_key = ${luaString(hyprlandSubscriptionKey)}
      if _G[subscription_key] ~= nil then
        _G[subscription_key]:remove()
        _G[subscription_key] = nil
      end
      _G[${luaString(hyprlandLaunchArmedKey)}] = false
      _G[owner_key] = nil
    end
  `;
  await $`hyprctl eval ${code}`.quiet().nothrow();
}

async function build(target: BuildTarget): Promise<boolean> {
  if (target === "daemon") {
    return buildDaemon();
  }

  console.log(`[waku-dev] Building ${isMacOS ? "app bundle" : "app"}...`);
  if (!(await buildDaemon())) {
    console.error(
      "[waku-dev] Daemon build failed; keeping the current app open.",
    );
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
    console.error(
      "[waku-dev] Daemon build failed; keeping the current daemon running.",
    );
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
  void launchedApp.exited.then(async (exitCode) => {
    if (stopping || app !== launchedApp) return;
    app = undefined;
    stopping = true;
    closeWatchers();
    clearRebuildTimer();
    await releaseHyprlandRules();
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

function targetForChange(
  directory: string,
  filename: string | Buffer | null,
): BuildTarget {
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
      (_eventType, filename) =>
        scheduleBuild(targetForChange(directory, filename)),
    );
    watcher.on("error", reportWatcherError);
    watchers.push(watcher);
  }

  const rootWatcher = watch(root, (_eventType, filename) => {
    if (filename && watchedFiles.includes(filename.toString()))
      scheduleBuild("app");
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
      if (!stopping) await prepareHyprlandLaunch();
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
  await releaseHyprlandRules();
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
  await prepareHyprlandLaunch();
  if (!stopping) app = launchApp();
} else {
  console.log(
    "[waku-dev] Changes arrived during the initial build; waiting to rebuild.",
  );
  if (queuedBuild !== undefined) void drainBuildQueue();
}

console.log(
  "[waku-dev] Watching for source changes. Daemon-only edits hot-reload without relaunching Waku.",
);
