# Contributing to Waku

Thanks for helping improve Waku. Bug reports, focused fixes, tests, and
well-scoped features are welcome.

## Development setup

The debug app requires:

- macOS, Linux (Wayland or X11), or Windows 10 1809 and newer
- Rust 1.96 or newer
- Bun
- A supported agent CLI when testing a provider integration

On Ubuntu and Debian, install the Linux compiler and GPUI runtime
prerequisites with:

```sh
sudo apt install build-essential clang cmake pkg-config libfontconfig-dev \
  libwayland-dev libx11-xcb-dev libxkbcommon-x11-dev libvulkan1 \
  xdg-desktop-portal
```

Equivalent packages are available on Fedora, Arch, and other desktop Linux
distributions. A working Vulkan driver is required at runtime.

Install dependencies and start the development watcher from the repository
root:

```sh
bun install
bun run dev
```

On macOS the watcher builds and signs `target/debug/Waku Debug.app`; on Linux
and Windows it builds `target/debug/waku`. In both cases the provider daemon remains an
external `target/debug/waku-debug-daemon`: provider-only edits rebuild and
hot-swap that process without relaunching the app, while desktop edits rebuild
and relaunch the app normally. Keep that watcher running while you work. Do
not start a second watcher or manually relaunch the debug app. Press `Ctrl-C`,
or quit the app, to stop it.

The embedded browser and experimental computer-use integration are currently
macOS-only. On Linux and Windows the browser reports that it is unavailable,
while the computer-use UI and runtime stay disabled.

Windows needs the MSVC toolchain (Visual Studio Build Tools with the C++
workload and the Windows SDK) so Cargo can link and so the resource compiler
is available for the executable's icon and version block.

## Linux bundle

To produce a distro-compatible release archive with the desktop and daemon
binaries, desktop entry, icon, and license:

```sh
./scripts/bundle-linux.sh
```

The archive is written under `target/release` with an install-prefix layout
(`bin/` and `share/`) beneath one versioned directory. It intentionally does
not bundle system graphics libraries; distribution packages should declare
those runtime dependencies normally.

`website/public/install.sh` (served at `https://waku.sh/install.sh`) is what
users run to install that archive. Point it at a local build to exercise it
without publishing:

```sh
WAKU_BUNDLE_PATH=target/release/waku-<version>-<target>.tar.gz \
  sh website/public/install.sh
```

[docs/linux.md](docs/linux.md) documents both paths for users.

## Windows bundle

To produce the portable archive and the installer, on Windows:

```sh
bun scripts/bundle-windows.ts
```

Both land under `target/release`. The zip holds the two executables side by
side beneath one versioned directory — the layout Waku needs to find its
daemon — and the installer is built from
[`resources/windows/waku.iss`](resources/windows/waku.iss), so Inno Setup 6.3
or newer must be installed (`choco install innosetup`) — the architecture
gate uses identifiers added in 6.3. Set `WINDOWS_CERTIFICATE`
(base64 `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD` to Authenticode-sign them;
without those the script packages unsigned binaries and says so.
[docs/windows.md](docs/windows.md) documents installing for users, and
[RELEASING.md](RELEASING.md) the signed update feed.

## Making changes

- Before starting work on anything larger than a bug fix, open an issue and
  discuss the proposal first.
- Keep changes focused and follow the existing Rust and GPUI conventions.
- Keep filesystem, process, network, and other blocking work off the UI thread.
  Rendering and row-building paths must read data already held in memory.
- Keep long collections virtualized and per-frame work proportional to visible
  content.
- Make every mouse control keyboard-operable, preserve visible focus, honor
  reduce-motion settings, and do not communicate state with color alone.
- Prefer provider-neutral behavior when a change applies to every agent, while
  preserving provider-native event order and session semantics.
- Add or update tests for behavior that can be verified without the UI.

## Checks

Run the focused checks relevant to your change, then run the full baseline
before opening a pull request:

```sh
cargo fmt --package waku --package waku-protocol --package waku-client --package waku-core --package waku-daemon -- --check
cargo check
cargo test
bun run protocol:check
bun run --filter @waku/client check
bun run --filter @waku/client test
```

When a Rust wire type changes, run `bun run protocol:generate` and commit the
updated files under `packages/waku-client/src/generated`.

For user-visible changes, wait for the watcher to report a successful rebuild
and validate the freshly relaunched app. Include screenshots or a short
recording in the pull request when they make the result easier to review.

## Pull requests

In the pull request description:

- Explain the problem and the chosen solution.
- List the checks you ran.
- Call out known limitations or follow-up work.
- Link the related issue, if one exists.

By contributing, you agree that your contribution will be licensed under the
[GNU General Public License v3.0 only](LICENSE).
