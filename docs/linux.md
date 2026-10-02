# Waku on Linux

## Install

```sh
curl -fsSL https://waku.sh/install.sh | sh
```

The script needs no root. It unpacks the release tarball into
`~/.local/waku.app` and installs the desktop entry into
`~/.local/share/applications`, so **Waku appears in your applications menu** —
you can also launch it from a terminal via `waku` command. Run the script again to
upgrade manually; the installed app also keeps itself current.

Waku expects:

- **A graphical session:** Wayland, or X11 with a running window manager.
  Launch the desktop app with `WAYLAND_DISPLAY` or `DISPLAY` set by that
  session. For an X11 virtual display, see the setup below.
- **glibc 2.35 or newer** — Ubuntu 22.04, Debian 12, Fedora 36, and anything
  more recent. Releases are built on Ubuntu 22.04, so older distributions must
  build from source.
- **A working Vulkan or OpenGL driver.** Waku renders through wgpu, which tries
  Vulkan first and falls back to GL. Software rasterizers (lavapipe, llvmpipe)
  are accepted, so it can run in a VM, but see the note below.
- **x86_64 or aarch64.** Other architectures build from source.
- `xdg-desktop-portal` for native file dialogs.
- `curl` or `wget` for installation and update downloads.

Set `WAKU_VERSION` to install a specific version rather than the latest.

## Displays and headless servers

`waku` is the desktop app. An SSH shell without display forwarding, a
headless container, or a system service usually has neither `DISPLAY` nor
`WAYLAND_DISPLAY`. Run the app from a graphical session; setting a display
name alone does not create an X server or Wayland compositor. Also unset
`ZED_HEADLESS`, which forces GPUI's headless backend even when a display is
configured. Use the separate `waku-daemon` executable to run only the agent
backend on a headless server.

On a bare X server such as Xvfb, start a window manager as well. Without one,
the app can create a mapped window that stays blank. `xvfb-run` starts the X
server, but does not start a window manager. For example, with `xvfb`,
`xauth`, and `openbox` installed:

```sh
xvfb-run -a -s '-screen 0 1600x1000x24' sh -c 'openbox >/dev/null 2>&1 & exec waku'
```

The virtual display still needs a working graphics driver (including a
software renderer); Xvfb and Openbox do not provide one.

## Installing manually

The script is a convenience, not a requirement. Download
`waku-<version>-<target>.tar.gz` from
[releases.waku.sh](https://releases.waku.sh) or the
[GitHub release](https://github.com/egoist/waku/releases), then unpack it
wherever you like:

```sh
mkdir -p ~/.local/waku.app
tar -xzf waku-<version>-<target>.tar.gz --strip-components=1 -C ~/.local/waku.app
ln -sf ~/.local/waku.app/bin/waku ~/.local/bin/waku   # optional
```

The archive uses an install-prefix layout (`bin/`, `share/`) beneath one
versioned directory, so `--strip-components=1` into a prefix such as
`/usr/local` works too.

**Keep `bin/` and `share/waku/` intact.** Waku launches its daemon, updater,
and Computer Use helpers from `bin/`; the SDK library and supporting resources
ship in the same installation. A symlink is fine — Waku resolves it back to
the real path.

Installing the desktop entry is the part that matters — it is how the app is
launched normally, and it is what associates the running window with its icon
and name (Waku reports the Wayland `app_id` / X11 `WM_CLASS` `sh.waku`, which
matches the entry's filename). Install the packaged file and point it at the
install (the packaged copy uses bare `Exec=waku` and `Icon=sh.waku` names so it
can be relocated):

```sh
install -D ~/.local/waku.app/share/applications/sh.waku.desktop \
  -t ~/.local/share/applications
sed -i "s|^Exec=waku$|Exec=$HOME/.local/waku.app/bin/waku|" \
  ~/.local/share/applications/sh.waku.desktop
sed -i "s|^Icon=sh.waku$|Icon=$HOME/.local/waku.app/share/icons/hicolor/256x256/apps/sh.waku.png|" \
  ~/.local/share/applications/sh.waku.desktop
```

## Updating

Tarball installs under the user's home directory update themselves. Waku
checks once per launch by default; an available release appears in the sidebar
footer. Clicking it validates the staged installation, quits through Waku's
normal draft/state saves, swaps the complete prefix, and relaunches. If the new
build exits before opening its window, the helper restores and relaunches the
previous version.

Every archive is verified with the same Ed25519 release key used by the macOS
and Windows updaters. The architecture-specific feeds are:

- `https://releases.waku.sh/appcast-linux-x86_64.xml`
- `https://releases.waku.sh/appcast-linux-aarch64.xml`

Use **Check for Updates** for an explicit check, or disable launch checks in
**Settings → General → Automatic updates**. System-wide installs such as
`/usr/local`, builds without the managed-install marker, root sessions, and
package-manager-owned builds do not modify themselves; upgrade those through
their original installation method. Re-running `install.sh` remains a safe
manual fallback for the default `~/.local/waku.app` install.

## Uninstalling

```sh
curl -fsSL https://waku.sh/install.sh | sh -s -- --uninstall
```

This removes `~/.local/waku.app`, the symlink, and the desktop entry. Projects
and settings stay in `~/.waku`; delete that directory to remove them too.

## Building from source

See [CONTRIBUTING.md](../CONTRIBUTING.md) for build prerequisites, then
install Bun for the SDK artifact assembler, then produce the same archive
this page installs with:

```sh
./scripts/bundle-linux.sh
```

To exercise the install script against that local build:

```sh
WAKU_BUNDLE_PATH=target/release/waku-<version>-<target>.tar.gz \
  sh website/public/install.sh
```

## Computer Use

Debug builds expose Computer Use through the bundled Cua Driver SDK. X11 and
AT-SPI use the current desktop session; native Wayland support is experimental
and depends on compositor integrations. See [Computer Use](computer-use.md)
for capability checks, packaged helper files, and limitations.

## Running in a virtual machine

VMs usually have no GPU passthrough, so Mesa falls back to a software
rasterizer. That works in principle — wgpu accepts a CPU adapter — but both
lavapipe (Vulkan) and llvmpipe (GL) JIT-compile shaders through LLVM, and that
path is fragile: on Fedora 44 aarch64 (mesa 26.0.3 + LLVM 22.1) it segfaults
inside `gallivm_jit_function` while compiling a fragment shader. The crash is
in the driver, not in Waku, and no application-side setting avoids it.

If the app dies on its first frame in a VM, check `coredumpctl info` for a
backtrace through `libvulkan_lvp.so` or `libgallium`. The reliable fix is to
give the guest a real GL driver — on UTM that means the QEMU backend with
virtio-gpu-gl (virgl) rather than Apple Virtualization, which offers Linux
guests no 3D at all. Hiding the Vulkan drivers lets wgpu try the GL path:

```sh
VK_DRIVER_FILES=/nonexistent.json VK_ICD_FILENAMES=/nonexistent.json waku
```

Set both spellings for compatibility: older Vulkan loaders, including
Ubuntu 22.04's 1.3.204, ignore `VK_DRIVER_FILES` and require
`VK_ICD_FILENAMES`. Newer loaders prefer `VK_DRIVER_FILES` when both are set;
the older spelling is deprecated but still supported. See the
[Vulkan loader's driver override documentation](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md#overriding-the-default-driver-discovery).
This selects a fallback path, not a replacement for a working GL driver.
