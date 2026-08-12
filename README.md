# Waku

Waku is a fast, native desktop app for working with local coding agents. It is
built in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts on your machine.

[Download Waku](https://waku.sh)

## Supported agents

Waku works with:

- [Amp](https://ampcode.com/)
- Claude Code
- Codex CLI
- Cursor CLI
- Grok Build
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

## CLI

The `waku` binary also exposes a local control surface for projects and sessions
(useful for agents and scripts). When the desktop app is running, commands talk
to it over a Unix socket beside the app database; otherwise they read and write
SQLite directly.

```sh
waku list-projects
waku new-project --path ~/code/my-app --name my-app
waku list-sessions --project my-app
waku new-session --project my-app --provider pi --model ...
waku open <project-or-session>
waku link-session --session <session-id> --project my-app
```

See `waku --help` and each subcommand's `--help` for flags.

## Development

Development currently requires macOS, [Rust 1.96 or newer](https://www.rust-lang.org/tools/install),
and [Bun](https://bun.sh/).

```sh
bun install
bun run dev
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and checks.
Release maintainers should also read [RELEASING.md](RELEASING.md).

## Sponsorship

You can support the project development via [GitHub Sponsors](https://github.com/sponsors/egoist).

## License

Waku is licensed under the [GNU General Public License v3.0 only](LICENSE).
