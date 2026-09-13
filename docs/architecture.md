# Architecture

Status: design. Numbers are marked **MEASURED** (prototypes and tests on the
test machine described in
open-url's design doc, test environment section (the unpublished prototype))
or **ESTIMATED**. Open items reference questions in
[decisions.md](decisions.md#open-questions).

## Core and plugins

The daemon is a generic core. It knows how to receive links, match them against
routes, run resolver chains with fallbacks, open content with targets, and it
offers services. It does not know about any content kind or site.

Plugins are Lua modules that use the core's API to add support for a kind of
content ([plugins.md](plugins.md)). The `mpv-video` plugin (YouTube, Instagram,
yt-dlp, a managed mpv) is the first; images, audio or documents would be
others, written the same way.

| Core (Rust) | Plugins and config (Lua) |
|---|---|
| Client, daemon, IPC, reload | Which links matter (routes) |
| Route matching, probing | How to turn a link into content (resolvers) |
| Fallback engine, trace | Which app opens it (targets) and how |
| Services: `http`, `json`, `xml`, `html`, `cache`, `exec`, `fs`, `probe`, `net`, `target`, `compositor`, `notify`, `log`, `env` | Policy on events (window placement, extra retries) |
| Connection pool, TLS sessions, streaming proxy | Settings for all of the above |
| App drivers (mpv, …), desktop-entry launch, compositor adapters | |

## Processes

```
  app (Telegram, kitty, browser portal, …)
        │  xdg-open / gio / portal OpenURI / KIO
        ▼
  default handler's desktop ID  resolved as usual; a same-ID shadow entry wins
        │  Exec = …/by-id/<id>   (a symlink to the link-router binary)
        ▼
  link-router (as by-id/<id>)   short-lived client, static binary
        │  Unix socket, one message, one ack byte
        ▼
  link-router daemon            long-lived, started on demand
   ├── Lua state                init.lua + plugins (first-party embedded, user in config dir)
   ├── route table              compiled from Lua declarations
   ├── fallback engine          resolver chains, failure families, route timeouts, trace
   ├── services                 http, json, xml, html, cache, exec, fs, probe, net, notify, …
   ├── connection pool          HTTP/2 and HTTP/1.1 connections, in-memory TLS sessions
   ├── streaming proxy          localhost HTTP server that prefetches streams for players
   ├── app drivers              managed apps: spawn, IPC connection, events (mpv first)
   ├── desktop launcher         desktop-entry Exec, activation token, no shell
   ├── sentinel                 keeps the shadow entries intact (integration.md#sentinel)
   └── compositor adapter       hyprland / kwin / none
        │
        ├── managed apps (e.g. one mpv, IPC held open by its driver)
        └── browser and other programs
```

### Client

The binary that shadowed desktop entries run, as `by-id/<id>`
([integration.md](integration.md#opening-through-a-shadow)). It does as little
as possible:

1. Read `CLOCK_MONOTONIC` (start of the link's trace).
2. Take `<id>` from `argv[0]`: the entry the links came through, and their
   fallback.
3. Handle the passthrough cases without the daemon, by `exec`ing the original
   entry: no arguments; arguments that aren't all URLs; any URL whose scheme
   isn't intercepted, or a `file://` URI; no config, or `enabled` not set in the
   state file.
4. Otherwise connect to `$XDG_RUNTIME_DIR/link-router/daemon.sock` and send one
   message: protocol version, the URL list, `fallback_id` (`<id>`), the start
   timestamp, the opener (basename of `/proc/<ppid>/exe`; the process-tree walk
   past launchers and scripts is **pending** Q10), and the environment variables
   the daemon needs per link: `XDG_ACTIVATION_TOKEN`, `DESKTOP_STARTUP_ID`,
   `WAYLAND_DISPLAY`, `DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE`,
   `XDG_CURRENT_DESKTOP`, `KDE_SESSION_VERSION`, `DBUS_SESSION_BUS_ADDRESS`.
5. Wait for the daemon's one-byte ack that it has taken the links, then exit.
   The client never waits for the result; `--wait` exists for debugging and
   prints the trace when the links are done.

The ack wait is bounded: after 1.5 s without an ack, the client closes the
socket and runs the original itself. The daemon takes a link only after its ack
write succeeds, so a link the client gave up on is never opened twice.

Started as `link-router open URL…` (from a terminal, without a shadow), the
client has no `fallback_id`; the daemon resolves the current default handler of
each URL's scheme and falls back to that entry's original.

Beyond steps 1–3, the client doesn't load the config or parse URLs beyond
validating them. It is the same binary as the daemon (one file to install), so
daemon code never runs at client start: no global constructors, no eager TLS,
Lua or runtime initialisation on the client path.

**MEASURED** with a prototype, exec to the message fully written: Rust musl
static 0.28 ms, glibc static 0.55 ms, glibc dynamic 0.85 ms; C glibc static
0.53 ms, musl static 0.61 ms. The bash handler it replaces takes 27 ms median
(21–34 ms, **MEASURED**).

If the socket doesn't accept the connection, the client starts the daemon and
hands it the message over an inherited pipe:
`link-router daemon --detach --open-fd <fd>`. The daemon writes the ack byte to
that pipe once it has taken the links; the client exits on the ack. If the pipe
reaches EOF without an ack, or the ack doesn't arrive within 1.5 s, the client
runs the original itself ([Failure handling](#failure-handling)). A static
daemon accepted connections 0.53 ms after spawn (**MEASURED**), so on-demand
start costs nothing noticeable. A lock on
`$XDG_RUNTIME_DIR/link-router/daemon.lock` stops two clients from starting two
daemons; the loser connects to the winner's socket. A daemon whose protocol
version differs from the client's (after an update) is told to exit, and the
client starts the new one.

`link-router open --no-daemon URL` runs the whole pipeline in the client
process, for debugging.

### Daemon: `link-router daemon`

Runs until `daemon.idle_exit` elapses with no link in progress, no managed app
open and no pending sentinel question (`false` keeps it resident). There is no
systemd assumption: on-demand start by the client is the only start mechanism
routing needs. Keeping interception intact after a default-handler change while
no daemon runs also needs a resident daemon or `link-router sentinel` in
session autostart ([integration.md](integration.md#sentinel), **pending** Q16).

Threads:

- **Async runtime** (tokio, multi-threaded): socket accept, services, the
  connection pool and proxy, drivers, timers, process management, compositor
  IPC.
- **Lua thread**: one Lua state, driven by a local task set. Resolvers,
  handlers and event callbacks run here; they call async Rust services through
  async functions, so a resolver waiting on HTTP doesn't block other links.
  Declarative routes are matched in Rust and never touch this thread.

The daemon doesn't use the environment it was started in: that is whatever
process happened to open the first link (a terminal, an AppImage with its own
`LD_LIBRARY_PATH`). It uses a canonical session environment instead, read
through `/proc` from a session process (the compositor, or the desktop portal):
`PATH`, `XDG_*` directories, `XDG_CURRENT_DESKTOP` (a colon-separated list),
locale. The sentinel resolves defaults and the desktop launcher starts programs
with it. Per-link values (activation token, display, compositor instance) come
from the client message, because the compositor can restart underneath a
long-lived daemon (a Hyprland crash gives it a new
`HYPRLAND_INSTANCE_SIGNATURE` and socket path); adapters reconnect when the
value they depend on changes, and the canonical environment is re-read then.

### Config reload

The config directory (including `plugins/`) is watched with inotify. On change
the daemon builds a new Lua state and runs `init.lua`; if it loads, routes,
resolvers, targets and settings are swapped in atomically and
`config.reloaded` fires. Links already in progress finish on the Lua state they
started on. If the load fails, the old config stays active and the error goes
to the log and a desktop notification. Managed apps are not restarted by a
reload ([integration.md](integration.md#app-drivers)).

## Connections and TLS

HTTP is Rust: hyper with rustls, root certificates compiled in (webpki roots),
HTTP/2 where the server offers it. **MEASURED** cold client CPU cost: rustls
1.5 ms, reqwest HTTP/2 3.0 ms, against 10.4 ms for the curl CLI; rustls loading
the system bundle at runtime costs 9.6 ms, so roots are compiled in and must be
kept up to date with releases. YouTube and Instagram served rustls, OpenSSL and
curl clients alike.

The daemon keeps a connection pool per origin and an in-memory TLS session
cache. Measured idle lifetimes decide the policy (**MEASURED**):

| Origin | Idle lifetime | Policy |
|---|---|---|
| `www.youtube.com` | GOAWAY after exactly 240 s; HTTP/2 PING doesn't extend it | keep the connection while it lives (a warm player request takes 136–165 ms vs 252–297 ms cold); a new connection after that |
| `*.googlevideo.com` | closed after 120 s; HTTP/1.1 only | no pool between clicks; `ctx.net.preconnect` opens two connections when a YouTube link arrives, in parallel with the player request (handshake 60–90 ms, the request 136–297 ms warm to cold) |
| `www.instagram.com` | closed after 65 s without traffic; with HTTP/2 PING every 30 s still open at 900 s | PING keepalive while the daemon runs; saves 100–170 ms of TCP+TLS per reel page |

TLS 1.3 0-RTT: `www.youtube.com` and `www.instagram.com` accept early data;
googlevideo doesn't. A session persisted to disk and replayed by another process
cut the player request's time to first byte from 270 ms (that test's own cold
baseline) to 212 ms (n=2); resumption
without early data saved nothing (**MEASURED**). Inside a running daemon the
connection pool already covers the 240 s window; 0-RTT would help the first
request after that window or after a daemon restart. rustls has no public API
to persist sessions across processes and its 0-RTT against Google is
unverified; an OpenSSL build (static, ~9 MB) can do it (**pending** Q1).

Requests support hedging (`opts.hedge`): an identical request on a new
connection after a delay, first answer wins, so a lost SYN (1 s retransmit)
doesn't cost a second (**pending** Q13 for the delay).

DNS: `ctx.net.warm_dns` resolves names in the background. It only speeds up
other processes (mpv opening streams without the proxy) when the system runs a
caching resolver; the test machine does (NetworkManager's dnsmasq), where cache
misses cost 30–215 ms (**MEASURED**). The daemon's own connections resolve
through the same cache.

## Streaming proxy

`lr-proxy` is a localhost HTTP server offered to drivers. A driver registers the
stream URLs of an item; the proxy starts fetching them immediately over the
pool's connections (always with `Range` requests: googlevideo paces requests
without `Range` to 32–80 KB/s, **MEASURED**) and serves the player from its
buffer, forwarding open-ended `Range` requests for seeks.

**MEASURED** with a prototype, mpv `loadfile` to first frame (vo=null):

| Path | First frame |
|---|---|
| mpv straight to googlevideo, parallel `audio-add` | 211–218 ms |
| Proxy, new upstream connections when URLs arrive | 151 ms (+8 ms to register) |
| Proxy, upstream connections already open, parallel `audio-add` | 90 ms (+7) |
| Proxy, upstream connections already open, EDL | 66 ms (+8) |
| All data already local (floor) | 13–25 ms |

- Through the proxy a plain EDL is the fastest way to load video and audio,
  because both fetches are already running when mpv opens the second stream.
- Memory: 1.3 MB RSS idle, 4.8 MB with 1.4 MB buffered; the prefetch window is a
  few MB per stream.
- Seeks worked; fetching beyond the prefetch window over the network was not
  tested.
- If the proxy fails for an item, the driver loads the direct URLs.

## Crates

One Cargo workspace, one installed binary.

| Crate | Responsibility |
|---|---|
| `link-router` | `main`: subcommand dispatch (`open`, `daemon`, `setup`, `enable`, `sentinel`, `uninstall`, `doctor`, `trace`, `lua-stubs`); started as `by-id/<id>`, it acts as the client with `<id>` as the links' fallback |
| `lr-core` | Link model, URL matching, probing, route table, content and action types, fallback engine, timing trace |
| `lr-lua` | Lua state (mlua), the `router` API, plugin loader, config reload, stub generation |
| `lr-ipc` | Client↔daemon protocol (versioned) and socket handling |
| `lr-net` | Connection pool, hyper + rustls client, session cache, hedging, DNS warming |
| `lr-services` | JSON, XML, HTML, cache, exec, fs, probe, notify, and the Lua-facing wrappers of `lr-net` |
| `lr-proxy` | Localhost streaming proxy for players |
| `lr-drivers` | Managed-app drivers behind one trait; `mpv` first (JSON IPC, events, the bundled mpv script) |
| `lr-desktop` | Desktop entry parsing, the desktop launcher, default-handler resolution in GIO order, canonical session environment, shadow entries, state file, sentinel checks and repairs, doctor checks |
| `lr-compositor` | Adapters: Hyprland (socket IPC), KWin (D-Bus), none |
| `plugins/` | First-party Lua plugins (`mpv-video`, …), embedded with `include_str!` |

Static linking: target `x86_64-unknown-linux-musl` for the release binary, with
Lua vendored (mlua's `vendored` feature) and rustls, so the binary has no
runtime library dependencies.

## Life of a link

```
client exec ──► socket write ──► daemon receive ──► ack
                                     │
                          event link.received (only if registered) ──► may return an action
                                     │
                          match routes in order (Rust; probe or Lua predicate only if a route needs one)
                                     │
            ┌──────────────┬─────────┴─────────┬──────────────┐
            ▼              ▼                   ▼              ▼
     resolve chain     target alone          exec        handler (Lua)
     + target.prepare()                                  → returns an action
     (first content
      returned wins;
      route timeout)
            │ content
            ▼
     target: exec / desktop entry / managed app driver (via the streaming proxy)
            │
     driver events ──► started / content_started / <driver events> / content_shown / content_ended / idle
            │                    │                             │
            │              plugin callbacks               error before content_shown ──► next resolver
            │              (e.g. window placement)        outside the producer's families, or the fallback
            ▼
     content_shown (trace ends: for mpv, playing and not paused for the cache)
```

Resolution runs before the target sees the link, so a managed app starts in
parallel with resolution, and resolver failures become fallbacks before any
window has content.

The timing budget of the video path is in
[plugins/mpv-video.md](plugins/mpv-video.md#timing-budget).

## Failure handling

Every stage that can fail has a next step, and the last step is always the
original handler the link came through (for `http`/`https` links, the browser
or the configured `browser`).

| Failure | Detected by | Next step |
|---|---|---|
| Daemon socket missing | client | start the daemon with the link |
| Daemon fails to start (pipe EOF without ack), or no ack within 1.5 s | client | run the original of the entry the client was started through ([desktop launcher](integration.md#desktop-launcher)); started without a shadow (`link-router open URL` from a terminal), resolve the current default for the URL's scheme and run its effective entry, or that entry's original if it is a marked shadow; if there is no handler, a desktop notification carrying the URL |
| Daemon protocol version differs from the client's | client | tell the daemon to exit, start the new one |
| The same URL comes back within 5 s of being handed out | fallback engine | straight to its fallback original, no routing (loop guard) |
| No route matches | core | the link's fallback original (implicit last route) |
| Resolver failure | fallback engine | next resolver; a `definitive` failure skips resolvers sharing a family with the failed one |
| Route `timeout` expires | fallback engine | the route's fallback |
| Chain exhausted | fallback engine | the route's fallback, by default the link's fallback original |
| Content kind has no target (`target` table) or is rejected by `accepts` | fallback engine | as a `definitive` failure of the producing resolver |
| `exec`/`desktop` target exits non-zero within its grace period | core | the route's fallback |
| Driver target fails to start, or its IPC dies | driver | the route's fallback for the current link; respawn on the next |
| Content fails in a managed app before `content_shown` | driver | as a `definitive` failure of the producing resolver, with the original link |
| Content fails after `content_shown` | driver | the item ends; no retry |
| Streaming proxy fails for an item | driver | load the direct URLs |
| Handler returns `nil` | core | the route's fallback |
| Lua error in a callback, handler or resolver | Lua thread | logged in the trace; handled as a failure of that step |
| Compositor adapter error | adapter | logged; content stays where the compositor put it |

The fallback engine records per link which resolvers it tried, so retries never
loop, and it always keeps the original URL (a managed app only sees resolved or
proxied URLs).

## Observability

- `link-router trace [N]`: the last N links with a per-stage timeline from the
  client's timestamp to `content_shown`, which route and resolver handled each
  link, and why the others failed. Stage boundaries are instrumented from the
  start because shortening them is a project goal.
- Log: `$XDG_STATE_HOME/link-router/daemon.log`, level from config.
- `link-router doctor`: checks that every path apps use to open links reaches
  link-router ([integration.md](integration.md#link-router-doctor)).

## Security

- The config and plugins are trusted code: they run with the user's privileges
  and can start processes.
- URLs and HTTP responses are untrusted. Nothing is ever passed through a
  shell; every process start takes an argv array. A placeholder such as `{url}`
  is substituted inside one argv element and never splits it.
- The daemon socket lives in `$XDG_RUNTIME_DIR/link-router/` with mode 0700; the
  daemon checks the peer's UID with `SO_PEERCRED`.
- The streaming proxy listens on loopback only and serves only URLs registered
  for current items, under random paths.
- The client forwards only the listed environment variables; everything else
  the daemon starts gets the canonical session environment, never the
  environment of the process that happened to start the daemon.
- 0-RTT early data is replayable; the only requests sent that way are the
  idempotent player/page requests.
