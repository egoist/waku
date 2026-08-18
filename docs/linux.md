# Waku on Linux

## Install

```sh
curl -fsSL https://waku.sh/install.sh | sh
```

The script needs no root. It unpacks the release tarball into
`~/.local/waku.app`, links `~/.local/bin/waku`, and installs the desktop entry
into `~/.local/share/applications`. Run it again to upgrade — it replaces the
previous install rather than merging into it.

Waku expects:

- **glibc 2.35 or newer** — Ubuntu 22.04, Debian 12, Fedora 36, and anything
  more recent. Releases are built on Ubuntu 22.04, so older distributions must
  build from source.
- **A working Vulkan driver.** GPUI renders through Vulkan and will not start
  without one.
- **x86_64 or aarch64.** Other architectures build from source.
- `xdg-desktop-portal` for native file dialogs.

Set `WAKU_VERSION` to install a specific version rather than the latest.

## Installing manually

The script is a convenience, not a requirement. Download
`waku-<version>-<target>.tar.gz` from
[releases.waku.sh](https://releases.waku.sh) or the
[GitHub release](https://github.com/egoist/waku/releases), then unpack it
wherever you like:

```sh
mkdir -p ~/.local/waku.app
tar -xzf waku-<version>-<target>.tar.gz --strip-components=1 -C ~/.local/waku.app
ln -sf ~/.local/waku.app/bin/waku ~/.local/bin/waku
```

The archive uses an install-prefix layout (`bin/`, `share/`) beneath one
versioned directory, so `--strip-components=1` into a prefix such as
`/usr/local` works too.

**Keep `bin/` intact.** Waku launches `waku-daemon` from its own directory, so
copying `bin/waku` somewhere on its own leaves it unable to start the daemon.
A symlink is fine — Waku resolves it back to the real path.

For a launcher entry, install the packaged desktop file and point it at the
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

Waku does not update itself on Linux — Sparkle is macOS-only. Re-run the
install script to upgrade.

## Uninstalling

```sh
curl -fsSL https://waku.sh/install.sh | sh -s -- --uninstall
```

This removes `~/.local/waku.app`, the symlink, and the desktop entry. Projects
and settings stay in `~/.waku`; delete that directory to remove them too.

## Building from source

See [CONTRIBUTING.md](../CONTRIBUTING.md) for build prerequisites, then
produce the same archive this page installs with:

```sh
./scripts/bundle-linux.sh
```

To exercise the install script against that local build:

```sh
WAKU_BUNDLE_PATH=target/release/waku-<version>-<target>.tar.gz \
  sh website/public/install.sh
```

## Known gaps

The embedded browser and computer-use integration are macOS-only. On Linux the
browser reports that it is unavailable and the computer-use UI stays disabled.
Agent sessions, projects, transcripts, skills, usage, diffs, file editing, and
the terminal all run natively.
