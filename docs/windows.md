# Waku on Windows

## Install

Download `Waku-<version>-x86_64-Setup.exe` (or the `aarch64` installer on an
Arm device) from [releases.waku.sh](https://releases.waku.sh) or the
[GitHub release](https://github.com/egoist/waku/releases) and run it. It
installs per-user into `%LOCALAPPDATA%\Programs\Waku`, so it never asks for
administrator rights — which is also what lets Waku update itself later
without a UAC prompt.

`https://releases.waku.sh/latest-windows.txt` names the current version if you
want to script the download.

### Portable

`waku-<version>-<target>.zip` is the same build without an installer. Unpack it
anywhere and run `waku.exe`.

**Keep the two executables together.** Waku launches `waku-daemon.exe` from its
own directory, so moving `waku.exe` out on its own leaves it unable to start
the daemon. A shortcut is fine.

A portable copy still updates itself: the updater passes the running
directory to the installer, so it replaces that copy in place rather than
creating a second install.

Waku expects:

- **Windows 10 version 1809 or newer**, or Windows 11.
- **A Direct3D 11 driver at feature level 11_0 or newer.** GPUI renders
  through DirectX and falls back to the Microsoft Basic Render Driver, so it
  can run in a VM — see Troubleshooting if the window comes up black.
- **x86_64 or aarch64.**

SmartScreen may warn about an unrecognized publisher on first launch when the
release was not code-signed. Choose **More info → Run anyway**.

## Updating

Waku updates itself. It checks once per launch, and an available update
appears in the sidebar footer; clicking it downloads the installer, verifies
its signature, and runs it. Waku closes, is replaced in place, and reopens.
Turn the check off in **Settings → General → Automatic updates** — **Check for
Updates…** in the app menu still works either way.

Updates are the same signed feed macOS uses, with one appcast per
architecture:

- `https://releases.waku.sh/appcast-windows-x86_64.xml`
- `https://releases.waku.sh/appcast-windows-aarch64.xml`

Every installer carries an EdDSA signature, and Waku refuses one that does not
verify against the public key built into it — so a compromised mirror or a
tampered download cannot install anything. The preference itself lives in
`%LOCALAPPDATA%\Waku\updater.json`.

## Where Waku keeps its data

| What | Path |
| --- | --- |
| Tasks, sessions, transcripts | `%LOCALAPPDATA%\Waku\app.db` |
| Attachments and blobs | `%LOCALAPPDATA%\Waku\blobs` |
| Settings | `%USERPROFILE%\.waku\app.json` |

Unpacking a new release over the old directory leaves all of it untouched.

## Agent CLIs

Waku detects the provider CLIs on `PATH` and, because a fresh `PATH` may
predate an install, also looks in the usual per-user prefixes:
`%APPDATA%\npm`, `%USERPROFILE%\.bun\bin`, `%USERPROFILE%\.cargo\bin`,
`%USERPROFILE%\scoop\shims`, and `%LOCALAPPDATA%\Microsoft\WindowsApps`.

Bare names resolve through `PATHEXT`, so the `claude.cmd` shim npm installs is
found the same way `claude` would be in a shell. Nothing is spawned with a
console window attached.

If a CLI is installed but not detected, set its path explicitly in
**Settings → Providers**.

## Terminal

The built-in terminal opens PowerShell 7 (`pwsh.exe`) when it is installed,
then Windows PowerShell, then whatever `COMSPEC` names. Ctrl+Shift+C and
Ctrl+Shift+V copy and paste so Ctrl+C stays available to the shell.

## What is not available yet

- **The embedded browser surface.** It reports that it is unavailable, as on
  Linux.
- **Computer use.** The runtime and its UI stay disabled off macOS.
- **Terminals over the daemon's browser client.** The desktop terminal works;
  a remote browser client connected to a Windows daemon cannot open one.

## Troubleshooting

**The window opens black, or the app exits at startup.** Waku needs a working
Direct3D 11 device. Update the GPU driver; in a VM, enable 3D acceleration.

**A provider is listed as not installed.** Open a new PowerShell window and run
the CLI by name. If the shell cannot find it either, the install did not put a
shim on `PATH`. If the shell finds it but Waku does not, set the binary path in
**Settings → Providers** and file an issue with the install method.

**Git-backed features do nothing.** Waku shells out to `git`. Install Git for
Windows and make sure `git --version` works in a new terminal.

**The update never arrives.** Waku reaches the feed with the `curl.exe` in
System32; a proxy or filter that blocks `releases.waku.sh` blocks updates too.
**Check for Updates…** reports the reason, where the once-per-launch check
stays quiet. Downloading the installer by hand and running it is always
equivalent.
