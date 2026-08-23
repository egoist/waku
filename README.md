# Waku

Waku is a fast, native desktop app for working with local coding agents. It is
built in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts on your machine.

## Install

On macOS, [download the signed `.dmg`](https://waku.sh). It updates itself.

On Linux:

```sh
curl -fsSL https://waku.sh/install.sh | sh
```

The script installs into `~/.local` without root. See
[docs/linux.md](docs/linux.md) for requirements, manual installation, and
uninstalling.

On Windows, run `Waku-<version>-<arch>-Setup.exe` from the
[latest release](https://github.com/egoist/waku/releases/latest). It installs
per-user and updates itself. A portable `.zip` is published alongside it. See
[docs/windows.md](docs/windows.md) for requirements and what is not available
there yet.

## Supported agents

Waku works with:

- [Amp](https://ampcode.com/)
- Claude Code
- Codex CLI
- Cursor CLI
- [Fx](https://fx.sh/)
- Grok Build
- Kimi Code
- OpenCode
- Pi

Install and authenticate at least one supported agent CLI before starting Waku.
Waku detects available CLIs automatically and uses each provider's native
structured protocol and session continuity.

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

Development is supported on macOS, Linux, and Windows and requires
[Rust 1.96 or newer](https://www.rust-lang.org/tools/install) and
[Bun](https://bun.sh/). Linux supports both Wayland and X11, and Windows needs
the MSVC toolchain; install the native build prerequisites listed in
[CONTRIBUTING.md](CONTRIBUTING.md) first.

```sh
bun install
bun run dev
```

The embedded browser and experimental computer-use integration currently
remain macOS-only. Agent sessions, projects, transcripts, skills, usage,
diffs, file editing, and the terminal run natively on Linux and Windows.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and checks.
Release maintainers should also read [RELEASING.md](RELEASING.md).

## Sponsorship

You can support the project development via [GitHub Sponsors](https://github.com/sponsors/egoist).

## License

Waku is licensed under the [GNU General Public License v3.0 only](LICENSE).
