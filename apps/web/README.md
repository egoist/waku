# Waku Web

Browser client for an existing Waku daemon. The Cloudflare Worker serves the
TanStack Start application only; it does not start, proxy, or store credentials
for a daemon.

```sh
bun --filter @waku/web dev
```

Then add a daemon WebSocket URL and token in the connection screen. The daemon
must be reachable through `wss://` in production and started with this site's
exact browser origin in its `--allow-origin` list. For local development the
origin is `http://localhost:3001`.

```sh
WAKU_DAEMON_TOKEN=replace-me cargo run -p waku-daemon --bin waku-daemon -- \
  --bind 127.0.0.1:34123 \
  --allow-origin http://localhost:3001
```

Waku Desktop can expose the daemon it manages from Settings → Daemon, where you
choose the port and exact browser origins and copy the URL/token. A standalone
daemon requires the explicit `--allow-non-loopback` flag for a non-loopback
bind. For access outside a private network, put a trusted TLS reverse proxy or
tunnel in front of the listener, forward WebSockets, and use `wss://`.

```sh
bun --filter @waku/web build
bun --filter @waku/web deploy
```

The token is a full-control daemon capability. It is sent directly from the
browser to the configured daemon and is never submitted to the Cloudflare
Worker. Persistent connections use browser local storage; leave “Remember” off
to keep it in session storage instead.

Task state, transcripts, workspaces, Git operations, attachments, and provider
processes all remain daemon-owned. The browser only keeps rendered query state
and composer input in memory.
