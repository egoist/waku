# Waku

Waku is a fast, native app for working with local coding agents. It is built in
Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts on your machine.

## Install

On Windows, run `Waku-<version>-x64-Setup.exe` from the
[latest release](https://github.com/yaffalhakim1/waku/releases/latest). It
installs per-user. A portable `.zip` is published alongside it, and an
`aarch64` build ships for ARM machines. See
[docs/windows.md](docs/windows.md) for requirements and what is not available
there yet.

Android is in progress: the app builds a release APK, but it is not published
yet and is not part of these releases.

macOS and iOS are not built. The desktop app was native on macOS and the
project still carries that code, but nobody maintains or ships those builds
here, so treat them as unavailable rather than broken.

Updates are manual: Waku ships no auto-updater and no update feed, so install
a new release from the releases page when you want one.

## Supported agents

Waku works with:

- [OpenCode](https://opencode.ai) — the focus of this fork
- [Amp](https://ampcode.com/)
- Claude Code
- Codex CLI
- Cursor CLI
- [Fx](https://fx.sh/)
- Grok Build
- Kimi Code
- Pi

Install and authenticate at least one supported agent CLI before starting Waku.
Waku detects available CLIs automatically and uses each provider's native
structured protocol and session continuity.

The desktop app keeps working with every provider above, but development
effort goes to OpenCode: it is the one that is exercised, and the one whose
sessions are expected to keep working.

## Highlights

- Keep projects and independent agent sessions in one native app.
- Switch models, reasoning effort, and access modes from a shared interface.
- Queue or steer follow-up messages while an agent is working.
- Rewind Git-backed tasks with conversation-aware checkpoints.
- Store app state locally, with no Waku account or remote service required.

## Architecture

The native desktop is an RPC client of the standalone `waku-daemon` process.
Provider sessions run in [`waku-core`](crates/waku-core), behind the
authenticated, versioned WebSocket contract in
[`waku-protocol`](crates/waku-protocol). Waku Desktop depends on
[`waku-client`](crates/waku-client), not on the daemon implementation. The
daemon owns task SQLite data, uploaded attachments, provider-native session
forks, and all workspace filesystem and Git operations; paths returned by it
always refer to the daemon host. The desktop retains only presentation state
and a disposable preview cache.

The browser client lives at [`apps/web`](apps/web) and uses the generated
browser transport in [`packages/waku-client`](packages/waku-client). Its
checked-in types are generated directly from the Rust protocol, while its
WebSocket client implements the same handshake, request IDs, subscriptions,
sequence deduplication, and replay cursors as the Rust client. Run
`bun run protocol:generate` after changing a wire type and
`bun run protocol:check` to verify that generated files are current.

Projectless task workspaces live on the daemon host under
`~/.waku/projects/<date>/<slug>`. The daemon moves workspaces created by the
older `~/.waku/<date>/<slug>` layout on first load.

Configuration ownership is separate too: the Release desktop writes
`~/.waku/app.json`, while Debug stays isolated at `temp/app.json`. Daemon
provider and Computer Use settings live in `~/.waku/settings.json`. The
desktop's Settings → Daemon page can explicitly
expose the child daemon on a fixed port, configure exact browser origins, and
copy its stable authentication token. It remains loopback-only by default.

When connected to a daemon managed outside the desktop process, Waku never
interprets daemon paths on the client machine. The local folder picker and PTY
are therefore unavailable until the protocol gains daemon-host picker and
terminal-stream endpoints; files, diffs, Git, skills, usage, task state, and
attachments already use daemon RPC.

Release apps bundle and sign `waku-daemon`. Development keeps the daemon at
`target/debug/waku-debug-daemon`, allowing provider-only edits to rebuild and
replace the daemon without relaunching Waku Debug.

## Development

Development is supported on Windows and requires
[Rust 1.96 or newer](https://www.rust-lang.org/tools/install) and
[Bun](https://bun.sh/). Windows needs the MSVC toolchain; install the native
build prerequisites listed in [CONTRIBUTING.md](CONTRIBUTING.md) first.

```sh
bun install
bun run dev
```

The embedded browser and experimental computer-use integration remain
macOS-only and are not built here. Agent sessions, projects, transcripts,
skills, usage, diffs, file editing, and the terminal run natively on Windows.

## Releasing

Releases are cut from GitHub Actions on a `v*` tag, or manually from the
**Actions** tab. The desktop workflow builds Windows only; a separate
**Android release** workflow assembles the APK and attaches it; there is no
update feed or artifact upload to any bucket. See [RELEASING.md](RELEASING.md)
for what is still upstream's and what this fork replaced.

## Upstream

This fork is Windows and Android only. The project it came from,
[egoist/waku](https://github.com/egoist/waku), is the one that ships builds
for **macOS, Linux, and Windows** — if you are on macOS or Linux, use that
release instead:

- [Upstream releases](https://github.com/egoist/waku/releases/latest)
- [waku.sh](https://waku.sh) — signed macOS `.dmg`, and `curl -fsSL https://waku.sh/install.sh | sh` on Linux

All credit for Waku goes to [egoist](https://github.com/egoist), who wrote it
and continues to develop it. Nothing here is monetized; if you want to support
the work, support upstream via
[GitHub Sponsors](https://github.com/sponsors/egoist).

## Fork notice

This repository is a fork of [egoist/waku](https://github.com/egoist/waku) and
is licensed under the same terms — [GNU General Public License v3.0
only](LICENSE). It is not affiliated with or endorsed by upstream.

Modifications to the original work (GPLv3 §5a):

- **2026-09-08** — `7bd92d1` Reconnect to a live local daemon after the client
  socket drops (`crates/waku-client`).
- **2026-09-09** — Mobile parity (branch `feat/mobile-parity`, since merged):
  CI typecheck and unit tests for `@waku/mobile`; rewind and fork; `/resume`
  for external provider sessions; a usage screen for spend and plan limits —
  all under `apps/mobile/`.
- **2026-09-09** — Desktop work in progress: agent presets, OpenCode 2
  sessions, model catalog, and regenerated protocol bindings under `crates/`,
  `src/`, and `packages/waku-client/src/generated/`.
- **2026-09-11** — `6424b2c` Refresh a resumed session from its provider
  transcript, so a session continued in the OpenCode CLI or another client
  shows those turns; OpenCode's history is read from its own server instead of
  an ACP replay that fails on real sessions.
- **2026-09-11** — `6c744e5` Ship no update feed. The updater pointed at
  upstream's `releases.waku.sh` and could replace an install with an upstream
  binary. The updater no longer initializes.
- **2026-09-11** — Release automation reduced to what this fork ships:
  `.github/workflows/release.yml` builds Windows only, `release-android.yml`
  builds the APK, and `sync-release.yml` (the R2 upload) is removed.

Bundled fonts under `assets/fonts/` are third-party and stay under their own
license ([MIT](assets/fonts/LICENSE-nerd-fonts.txt)).

## License

Waku is licensed under the [GNU General Public License v3.0 only](LICENSE).
