# link-router

Status: 0.1.0-beta.4. Interception and the `mpv-video` plugin work; most of the
design below is not implemented yet (see [MVP status](#mvp-status)).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/install.sh | sh
```

Without piping a remote script into a shell: download the archive for your
system from the [releases](https://github.com/v-dermichev/link-router/releases)
(`link-router-<version>-x86_64-unknown-linux-musl.tar.gz` or
`...-x86_64-unknown-freebsd.tar.gz`) and `SHA256SUMS`, then

```sh
sha256sum -c --ignore-missing SHA256SUMS   # Linux and FreeBSD alike
tar xzf link-router-*.tar.gz && cd link-router-*/
less install.sh                            # it's plain POSIX sh
./install.sh                               # uses the binary in this directory
```

The installer:

1. asks which links should play in mpv: YouTube, Instagram, direct media links
   (`.mp4`, `.webm`, ...), or takes `--youtube`, `--instagram`, `--direct`,
   `--all`. The choice is a marked block at the end of `init.lua`; rerunning the
   installer offers to change it and leaves the rest of the config alone;
2. checks requirements (Linux x86_64 or FreeBSD 15 amd64, `curl` or `wget`,
   `sha256sum`, `mpv`; `yt-dlp` optional) and offers to install missing
   packages with pacman, apt, dnf, zypper, xbps, apk or pkg;
3. warns when the default `https` handler isn't a web browser (link-router
   falls back to it, so make the browser the default first);
4. downloads the static release binary to `~/.local/bin` and verifies its
   checksum;
5. writes `~/.config/link-router/init.lua` if there is none;
6. runs `link-router enable`;
7. registers the daemon to run at login: a systemd user unit, an OpenRC user
   service (`rc-update --user`), or, when neither is running, an XDG autostart
   entry plus starting the daemon now. Compositors that ignore XDG autostart
   (Hyprland, sway, niri, river) need
   `link-router daemon --resident` in their own startup list.

Run the same command again to upgrade: with link-router already installed
(found through its recorded install path, so a custom `--bin-dir` is kept),
the installer only downloads when the version differs, asks once, swaps the
binary, keeps the config and interception as they are (it offers to change
which links play in mpv), refreshes the shadow entries and restarts the
registered service. `--reinstall` runs the full installation instead.

A resident daemon matters: its sentinel re-points interception when a browser
makes itself the default. Without it (`--service none`) the first link starts
the daemon, which exits after 30 idle minutes.

Nothing outside your home directory changes except packages you agree to
install. Options go after `sh -s --`, e.g. `| sh -s -- --yes --all`: `--yes`,
`--no-enable`, `--no-config`, `--no-deps`, `--service KIND`, `--version V`,
`--bin-dir DIR`, `--reinstall`; `sh install.sh --help` lists them.

Uninstall (restores the browser entries, removes the service, binary, data and
logs; keeps the config unless `--purge`):

```sh
curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/uninstall.sh | sh
```

## Shape in one paragraph

A tiny statically linked client, `link-router`, is what the shadowed
default-handler entries run (through per-entry `by-id/<id>` symlinks), so it
receives the links of the intercepted schemes. It passes a link straight to the
original handler when there is nothing to route, and otherwise hands it to a
long-lived daemon over a Unix socket and exits, starting the daemon first if it
isn't running. The daemon is a generic core:
it loads the Lua config and plugins once, matches links against routes in
Rust, runs resolver chains with fallbacks, and offers services that plugins
build on (HTTP with warm connections, a streaming proxy that prefetches media
for players, parsing, caches, running programs, desktop entries, drivers for
controllable apps such as mpv, compositor adapters). The core knows nothing
about video or any site; plugins do.

## Documents

| Document | Contents |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Processes, core vs plugins, connections and TLS, streaming proxy, crates, the life of a link, failure handling |
| [docs/lua-api.md](docs/lua-api.md) | The `router` Lua API used by configs and plugins |
| [docs/plugins.md](docs/plugins.md) | Plugin model: structure, loading, names, overriding, first-party plugins |
| [docs/plugins/mpv-video.md](docs/plugins/mpv-video.md) | The `mpv-video` plugin: resolvers for YouTube, Instagram and yt-dlp, the mpv target, player settings, timing budget |
| [docs/integration.md](docs/integration.md) | Interception (shadow entries, sentinel, state, doctor), app drivers (mpv), desktop launcher, compositor adapters |
| [docs/testing-kde.md](docs/testing-kde.md) | Checklist for a first test on KDE Plasma |
| [docs/decisions.md](docs/decisions.md) | Decision log, prior art, rejected alternatives, open questions |
| [examples/init.lua](examples/init.lua) | A config reproducing `open-url`, plus three inline routes |
| [examples/hyprland-rule.lua](examples/hyprland-rule.lua) | Hyprland window rule for the video player |

## MVP status

Build from source: `CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --target x86_64-unknown-linux-musl`,
then `sh install.sh --binary target/x86_64-unknown-linux-musl/release/link-router`.
Releases attach that binary and a FreeBSD 15 build (cross-compiled for
`x86_64-unknown-freebsd` against a FreeBSD sysroot), each with its `.sha256`. Tests: `cargo test`, and for the
KWin script `link-router kwin-script | node tests/kwin-placement.mjs`.

Implemented:

- Interception: `enable [--yes]`, `disable`, `sentinel`, `doctor`. Shadow
  entries, `by-id` links, `state.json`; user overrides are moved to
  `originals/` only with consent. The daemon runs the sentinel at start and on
  inotify changes of mimeapps lists and application directories.
- Client: passthrough (no URLs, non-URI args, non-http(s), disabled, no
  config) execs the original entry; otherwise it hands the link and its whole
  environment to the daemon, starting it on demand, and exits (about 1 ms warm,
  2 ms with a daemon start). mpv and the browser fallback run with that
  environment, so a daemon started by a service manager behaves like the app
  the link came from.
- Daemon: `link-router daemon --resident` for service managers never idles
  out and takes over from a daemon a click started; `link-router stop`
  restarts it under a supervisor.
- Config: `router.config({ daemon = { idle_exit } })` and
  `router.use("mpv-video", { player, quality, window, sites })`. Every other
  `router.*` call is accepted and logged as not implemented.
- `mpv-video`: YouTube direct (visionos player API), Instagram direct (reel
  page DASH manifest), yt-dlp fallback, and direct media links (a URL path
  ending in `.mp4`, `.webm`, `.mkv`, `.mov`, `.m4v`, `.ogv`, `.avi`, `.flv`,
  `.3gp` or `.m3u8`, streamed as is; `sites.direct` takes `false` or a list);
  one mpv over JSON IPC, reused for later links; audio added before the first
  frame; idle quit.
- Placement: with Hyprland, a runtime window rule sets the floating size and
  position before mpv is started, so the first map already has the final
  geometry; a running player is resized and moved before the next file loads.
  On KDE Plasma 6, mpv sizes the window from `geometry` and a KWin script
  loaded over D-Bus anchors it bottom-right, keeps it above other windows and
  follows the current desktop (not yet tested on a real Plasma session; see
  [docs/testing-kde.md](docs/testing-kde.md)). On sway, a `for_window` rule on
  the new player's PID floats, sizes and places it before it maps (the player
  is borderless there), and criteria commands move a running one. A player the
  user made fullscreen is left alone when the next link loads. Elsewhere mpv gets
  `--geometry=WxH-35-25`, which X11 window managers position and Wayland
  compositors use for the size only. Leaving fullscreen places the window
  again for the video playing at that moment.
- mpv output: unless the config chooses `--vo` or `--gpu-context`, a Wayland
  session gets `--vo=gpu-next,gpu,wlshm --gpu-context=waylandvk,wayland`, or
  `--vo=wlshm` when the compositor can't take GPU buffers (no
  `zwp_linux_dmabuf_v1`, e.g. wlroots' pixman renderer), where GPU outputs
  would render through llvmpipe. `link-router wayland-globals` shows what the
  compositor offers.
  When the size isn't known in advance (direct links, some yt-dlp results), the
  mpv script reports the demuxed size before mpv creates the window and holds
  playback until the window is placed.
- Failures: a `Definitive` or not-a-video resolver result, or a player that
  can't start, opens the original handler; transient resolver failures and
  streams that fail to play move on to the next resolver.

Not implemented: routes, Lua plugins and handlers, the streaming proxy,
window placement on GNOME (planned for the next version), the first-run
wizard.

Logs: `$XDG_STATE_HOME/link-router/daemon.log` (milliseconds since the click),
`mpv.log` next to it. The player's app ID defaults to `link-router-mpv`; the
runtime rule owns its placement, so no static window rule is needed.

Measured on this machine (1080p monitor, daemon warm): YouTube click to first
frame 0.28 s into a running player, 0.44 s with a new mpv; Instagram reel
1.1 s, 0.9 s of it fetching the page; a direct 460x344 `.webm` 0.2–0.35 s.

## Goals

- A generic, scriptable core; content kinds and sites are plugins.
- The shortest possible click-to-content time for the links a plugin handles;
  the core must never be the slow part.
- Never worse than without it: every failure ends in the original handler the
  link came through (for web links, the browser).
- Cross-compositor: compositor features are optional adapters.
- One config language for settings, routes and plugins.
- A single static binary to install, with no runtime dependencies besides
  the apps it opens things in.

## Non-goals

- Downloading media for keeping. yt-dlp and friends do that.
- A general-purpose extractor collection. First-party resolvers exist only
  where they are much faster than delegating (for video, to yt-dlp).
- Sandboxing untrusted third-party plugins. Plugins are trusted code, like the
  config.
- Windows or macOS.
