# Decisions

Status: design. Each entry records what was decided and why, or what is still
open and what would settle it. Measurements come from prototypes and tests
described in open-url's design doc (the unpublished prototype).

## Decided

### D1. Rust, one statically linked binary

The router replaces a bash script whose per-link cost is 21–34 ms of process
spawns (**MEASURED**), and it sits on the latency-critical path between a click
and an app showing content. A static Rust musl binary got from exec to the link
written on the socket in 0.28 ms (**MEASURED**), with no runtime library
dependencies and safe handling of untrusted URLs and HTTP responses. C reached
the same order of startup time (0.53–0.61 ms, **MEASURED**) with more risk in
parsing code. Client and daemon share one binary because installation, the
`by-id` symlinks of shadow entries and packaging then point at a single file.

### D2. Short-lived client plus on-demand daemon

A process started per link pays config loading, TLS setup and IPC connection
setup every time, and can't keep anything warm between clicks. The daemon
holds all of that; the client only writes a message to a socket and waits for a
one-byte ack. It starts on demand because the machine has no systemd (no socket
activation) and a resident process shouldn't be a requirement for routing; a
static daemon accepts connections 0.53 ms after spawn (**MEASURED**). Noticing a
default-handler change while no daemon runs is the one thing that needs a
resident daemon or `link-router sentinel` in autostart (D12, **pending** Q16).
A daemon is also what
lets drivers watch a running app's events (for mpv: window fitting, retries)
without the app deciding on its own when to quit.

Consequence: the daemon must survive compositor restarts and changing session
environments, so per-link values come from the client and everything else from
a canonical session environment (D13).

### D3. A generic core; content kinds are plugins

The core knows about links, routes, fallback chains and services, not about
video, YouTube or mpv. Everything content-specific (which sites, how to resolve
them, which app opens the result, how its window is placed) lives in plugins.
Video is the first plugin and the one the latency work targets; images, audio,
documents or anything else are the same kind of plugin, not special cases in
the core.

The core's job toward plugins is to make their hot paths cheap: matching, HTTP
with warm connections, parsing, caches, the streaming proxy, app drivers and
compositor calls are Rust services, so a plugin's own logic is orchestration.

### D4. Lua is both the config and the plugin language

One mechanism instead of two: a plugin is a Lua module that registers routes,
resolvers, targets and event handlers through the same `router` API the config
uses. The config either loads plugins (`router.use("mpv-video", opts)`) or declares
the same things inline; custom handling can start as a few lines in `init.lua`
and later move into a plugin file unchanged.

First-party plugins (`mpv-video`, and later `image`, `audio`, …) are Lua files
embedded in the binary. A plugin directory in the config
(`~/.config/link-router/plugins/<name>/init.lua`) with the same name takes
precedence, so shipped plugins can be copied and modified.

Declarative parts (route `match` tables, chains, settings) are compiled into
Rust structures when the config loads, so Lua costs nothing per link unless a
route calls a Lua function or a `link.received` callback is registered. When a
plugin's per-link Lua work turns out to be measurable, the fix is a core
service, not a native plugin.

It matches how the user's Hyprland is configured (`hyprland.lua`), and generated
LuaLS stubs give completion and type checks.

### D5. Resolution before the app sees the link

Resolvers turn a link into content (stream URLs, a downloaded file, a
canonical URL) in the daemon, then the core hands the result to the target app.
For video this replaces mpv's ytdl hook: resolution runs in parallel with player
start, failures become fallbacks before a window shows content, and retries
don't depend on the player. mpv runs with `--ytdl=no`; with the hook loaded, a
stream URL that fails to open goes through `yt-dlp -J` before mpv reports the
failure (~0.8 s later, **MEASURED**). The same shape serves other kinds: an
image plugin resolves a gallery page to image URLs, a document plugin downloads
a PDF with `core.download`.

### D6. Fallback chains end in the original handler

Fast resolvers depend on undocumented site behaviour and will break. Every
resolving route has an ordered chain; a broken fast resolver makes links slower,
not broken. Resolvers that send the same request share a family, so a
definitive rejection isn't repeated. The last step is always the original
handler the link came through (for web links, the browser), and a link no route
claims goes there directly.

### D7. Compositor support through optional adapters

Cross-compositor use (Hyprland now, Plasma possibly later) rules out compositor
calls in the core's logic. Adapters provide window placement and workspace
moves where available, as a service plugins call; without one, compositor rules
handle placement.

### D8. HTTP: hyper + rustls, roots compiled in

Cold client CPU cost (**MEASURED**): rustls 1.5 ms, reqwest HTTP/2 3.0 ms, libcurl
10.0 ms, curl CLI 10.4 ms; rustls loading the system trust store at runtime
9.6 ms. On the network the TLS library made no difference to the player request
(curl 264–297 ms, rustls HTTP/1.1 252 ms), and YouTube and Instagram treated
rustls, OpenSSL and curl the same. rustls keeps the binary static without a C
TLS library; its in-memory session cache covers resumption inside the daemon.
Roots are compiled in (webpki roots) and have to follow releases.

Persisting sessions across daemon restarts for 0-RTT needs OpenSSL
(**pending** Q1).

### D9. Connection policy from measured idle lifetimes

`www.youtube.com` closes idle connections at 240 s (GOAWAY; PING doesn't
extend it), googlevideo at 120 s (HTTP/1.1 only), `www.instagram.com` at 65 s
unless pinged, and with HTTP/2 PING every 30 s it stayed open past 900 s
(**MEASURED**). So the daemon keeps YouTube's connection while it lives, pings
Instagram's, and doesn't pool googlevideo between clicks: resolvers call
`ctx.net.preconnect` when a link arrives, which opens two googlevideo connections
while the player API request (136–297 ms warm to cold) is still running
(handshake 60–90 ms, **MEASURED**).

### D10. A streaming proxy in the core

Through a localhost proxy that starts both stream fetches as soon as the URLs
are known, mpv reached first frame 66 ms after `loadfile` on warm connections
and 151 ms on new ones, against 211–218 ms opening googlevideo directly with
parallel audio (**MEASURED**, vo=null). Seeks work, memory is a few MB, and a
plain EDL is the fastest load through it. It is a core service because any
player driver benefits, and drivers fall back to direct URLs if it fails.

### D11. Plain player defaults; machine-specific settings documented

The `mpv-video` plugin starts mpv with its defaults plus `--ytdl=no`. Settings that
won big on the test machine are documented as a recommendation in
[plugins/mpv-video.md](plugins/mpv-video.md#player-settings), not made defaults, because
they depend on the hardware and on the session environment: OpenGL instead of
Vulkan on a hybrid laptop whose displays are on the Intel GPU (exec to first IPC
reply 126–149 ms vs 697–784 ms, **MEASURED**), VA-API through Intel's `iHD`
driver (about two thirds less playback CPU, **MEASURED**), Mesa-only EGL, and
`--wayland-content-type=none` (the monitor blanked for seconds when mpv went
fullscreen with the default content-type hint; no other application triggered
it).

Stream-open options for items loaded without the streaming proxy are plugin
options: `tcp_nodelay=1` is on by default because it only removes Nagle delay
on the player's own connections; the one-certificate `--tls-ca-file` that
saved the rest of ~57 ms per open (**MEASURED**) stays a documented
recommendation, because the right certificate depends on the hosts and the
trick relies on mpv's `tls-verify` being off.

### D12. Intercept at the default handler's desktop entry, kept intact by a sentinel

The handler XDG resolves for each intercepted scheme stays what it is (for
`http`/`https`, the browser). link-router writes a same-ID shadow of that entry
in `$XDG_DATA_HOME/applications` whose `Exec` is a per-entry `by-id` symlink to
link-router; links fall back to the original entry; a sentinel re-checks and
repairs the shadows on daemon start, config reload, inotify events while the
daemon runs, and from session autostart
([integration.md](integration.md#interception)).

Requirements that decided it:

1. Browser-agnostic, with no per-browser setup after installing link-router.
2. The browser reads XDG as it is and believes it is the default, so it never
   prompts or takes links back; link-router's fallback is exactly that XDG
   default, and with no config everything passes through.
3. Every opening path is covered: `xdg-open`, GIO, the desktop portal
   (Telegram Desktop, GTK 4 `GtkUriLauncher`, Qt ≥ 6.12, Flatpak apps) and KIO.
4. A browser installed or made default later is covered too.

Only the shadow sits on the path all four opening mechanisms share (desktop ID
→ desktop file, `$XDG_DATA_HOME` first). Chromium-family browsers compare
desktop IDs in their default check, so the shadow keeps them satisfied. Firefox
and Zen compare the `Exec` binary instead; launching them through link-router
with `MOZ_APP_LAUNCHER` set to the current default's exact by-id path satisfies
them (verified in source; Q15 covers testing with real browsers). The sentinel
covers requirement 4, except while no daemon runs; a resident daemon or
`link-router sentinel` in autostart closes that (**pending** Q16).

Scope decisions:

- **Schemes.** Every scheme in the intercepted set is handled the same way: the
  default handler's entry is shadowed, and links fall back to its original. A
  scheme without a default handler is not intercepted: an entry of
  link-router's own could only become the default by editing `mimeapps.list` or
  by GIO's "any app with this MimeType" fallback, which is exactly the fragile
  registration this decision avoids. The config's `browser` overrides only
  `http`/`https` fallbacks, because sending a `mailto:` or `magnet:` link to a
  web browser would bounce it back to the system handler.
- **What gets routed.** Only links whose scheme is in the set reach the daemon;
  `file://` URIs and paths that arrive through a shadow (from "Open with" on a
  local file) go straight to the original from the client.
- **Same-binary shadows only for Gecko.** They exist because Firefox's default
  check compares binaries: every ordinary launch of a Gecko default must go
  through link-router to carry `MOZ_APP_LAUNCHER`. The executable is compared
  after skipping `env`, `flatpak run` and `/snap/bin` wrappers, the entry must
  list an http(s) MimeType, and entries with profile or app arguments are left
  alone. Chromium-family browsers compare IDs, so their many PWA and shortcut
  entries (written by the browser, rewritten on update) are never touched.
- **Browser-generated `userapp-*` entries** follow the repair policy instead of
  always asking: Firefox and Zen create them when "set as default" runs outside
  their entries, they aren't the user's work, and asking on each one would make
  `repair = "auto"` meaningless. A `userapp-*` shadow that stops being the
  default is removed and its original restored, so the next such run reuses
  that file instead of creating another.

Residual gaps:

- Firefox/Zen started outside their desktop entries prompt; accepting creates a
  `userapp-*` entry that the sentinel shadows, so routing continues.
- "Open with <default browser>" on an http(s) link is routed like a default
  open: at the desktop-entry layer the two can't be told apart (**pending**
  Q19).
- A default changed while no daemon runs is only noticed at the next daemon
  start or sentinel run (Q16).
- A millisecond race between a default change and the repair.
- Removing the binary without `link-router uninstall`: GIO, the portal and
  KService fall through to the original, but `xdg-open`'s generic mode goes to
  its fallback browser list.
- XFCE's `exo-open` uses its own helper configuration and isn't covered.

Upstream changes that would remove the need for shadows: Firefox comparing
desktop IDs like Chromium (`nsGNOMEShellService::IsDefaultBrowser`), or a
cross-desktop "URL pre-dispatch handler" in the MIME apps spec honoured by GIO,
KIO, xdg-utils and the portal (xdg-desktop-portal issue #472 is related). No
such proposal exists (**pending** Q18).

### D13. Interception safety rules

Rules that keep shadows from turning into a failure mode of their own
([integration.md](integration.md#interception)):

- **Explicit consent in a state file.** `$XDG_STATE_HOME/link-router/state.json`
  records `enabled`, the install path, autostart entries added, per-shadow
  hashes and the last sentinel result. Nothing is shadowed unless `enabled` is
  set, by `link-router setup` or, until the wizard exists, `link-router
  enable`. Without an explicit state, "until setup has run nothing is
  intercepted" couldn't be enforced by a sentinel that runs on every daemon
  start.
- **Uninstall order.** Stop the daemon, remove recorded autostart entries, set
  `enabled = false`, then remove shadows. Removing shadows first would let a
  running daemon with `repair = "auto"` re-create them.
- **Never clobber someone else's edit.** The state file keeps the hash of each
  shadow as written. A shadow edited since then is treated like a user file
  (always asked about), and uninstall only removes shadows that are unchanged
  and restores moved originals only over an unchanged shadow.
- **Canonical session environment.** The daemon may have been started by any
  process (a terminal, an AppImage). It reads `PATH`, the `XDG_*` directories,
  `XDG_CURRENT_DESKTOP` (a list) and locale from a session process through
  `/proc`, resolves defaults and launches programs with that, and never removes
  a shadow on a resolution it can't confirm with it. Otherwise a skewed
  `XDG_DATA_DIRS` could delete valid shadows and fallback browsers could start
  with a foreign environment.
- **Binary identity.** `by-id` symlinks point at the install path recorded in
  the state file, checked with `stat`; `/proc/self/exe` can name a deleted or
  different binary after an update, and two installs would otherwise keep
  repointing the links. The client↔daemon protocol is versioned; a mismatch
  restarts the daemon. A shadow gets `TryExec=<by-id path>` when its original
  has no `TryExec`, so GIO and KService skip it if the binary is gone.
- **Loop guard.** A URL that comes back within 5 s of being handed to an
  original or a target goes straight to its fallback original. Shadows make
  re-entry possible from any `exec` target that calls `xdg-open`, from a
  `desktop` target naming a shadowed ID, or from an app bouncing an unsupported
  scheme to the system handler. The desktop launcher also never runs an entry
  whose command is link-router.
- **Bounded client wait.** The client waits at most 1.5 s for the daemon's ack,
  then runs the original itself; the daemon takes a link only after its ack
  write succeeds, so the link isn't opened twice. A hung daemon must not block
  every browser link.
- **Desktop actions through `by-id`.** "New window" and "New private window"
  run the same executable as the main command, so they go through link-router
  too and carry `MOZ_APP_LAUNCHER` for Gecko; otherwise a first Gecko instance
  started from a dock action would prompt.
- **`$BROWSER` is a doctor warning, not a repair.** The sentinel can't change a
  session variable, and `BROWSER` pointing at link-router is fine.
- **inotify on directories.** Mimeapps writers use atomic renames and
  directories appear later, so the sentinel watches directories and their
  parents, ignores its own writes, and waits for 500 ms of quiet.
- **Download lifetime.** A `core.download` file is kept at least `keep` after
  launch (default 10 minutes), not only until the launched process exits:
  single-instance and D-Bus-activated apps exit at once after handing the file
  to a running instance.

## Prior art

Checked 2026-09-13. Nothing on Linux combines rule-based routing with
resolution, managed apps and fallbacks; the closest tools cover the routing
step only, which is a few milliseconds of the click-to-content time.

| Tool | What it is | Why it doesn't replace link-router |
|---|---|---|
| [handlr-regex](https://github.com/Anomalocaridid/handlr-regex) | Rust `xdg-open`/`xdg-mime` replacement; TOML handlers matched by regex, run with desktop field codes | Per-link process that only execs a command. Using it as the router adds a process in front of the resolver instead of removing one |
| [Switchyard](https://github.com/alyraffauf/switchyard) | Go + GTK4 browser launcher; TOML rules (domain, wildcard, regex), URL rewriting, picker when nothing matches | Browser-oriented, no scripting, no resolution or app control |
| [Linkquisition](https://github.com/Strobotti/linkquisition) | Go browser picker with JSON rules | Browser choice only |
| [Junction](https://github.com/sonnyp/Junction) | GNOME app chooser shown for each link | No rules |
| [mimi](https://github.com/BachoSeven/mimi) | `xdg-open` replacement script | No routing rules |
| [Finicky](https://github.com/johnste/finicky) | macOS browser router with a JavaScript/TypeScript config | macOS only; the closest model to link-router's scriptable config, browsers only |
| [mpv-handler](https://github.com/akiirui/mpv-handler) | Rust protocol handler (`mpv-handler://`) fed by a browser userscript; plays through mpv + yt-dlp | Only links clicked in a browser with the userscript; resolution is mpv's ytdl hook (the slow path) |
| [open-in-mpv](https://github.com/Tatsh/open-in-mpv) | Browser extension context-menu entry passing links to mpv | Same as above |
| [Grayjay](https://github.com/futo-org/Grayjay.Desktop) | Media app with JavaScript source plugins | A player application, not a link handler; its plugin-per-source model is similar to link-router's resolvers |
| [RustyPipe](https://codeberg.org/ThetaDev/rustypipe) | Rust client library for YouTube's Innertube API | A candidate dependency for YouTube resolution, not a router. Its docs say streams from web clients need PO tokens, generated by a separate `rustypipe-botguard` CLI; whether its other clients match the single `visionos` request's cost is unchecked |
| [jaro](https://github.com/isamert/jaro) | `xdg-open` replacement with a Scheme (Guile) config: regex routes, fallbacks, selection menus | Closest scriptable router on Linux; shadows `xdg-open`, so GIO and portal opens bypass it |
| [xdg-override](https://github.com/koiuo/xdg-override) | Per-application `PATH` with a copied `xdg-open` | Per app, `xdg-open` only |

How link routers deal with browsers taking the default back (research
2026-09-13, from source and issue trackers): on Linux, Junction, Switchyard,
Linkquisition and Braus make themselves the default (`gio mime`,
`xdg-settings`, GIO) and at most re-check when their own window opens;
handlr, jaro, mimi and xdg-override replace or shadow `xdg-open`, which misses
GIO and the portal; none hooks GIO, the portal or KIO, and none uses the OS
default as its fallback. Finicky re-asserts on each launch and macOS asks the
user to confirm. On Windows (Hurl, BrowserPicker, BrowseRouter) the user picks
the default in Settings and browsers can't silently reclaim it; Browser Tamer
runs a health check every 5 s and passes `--no-default-browser-check` to the
Chromium launches it makes. Android's LinkSheet holds the browser role, which
only a system dialog can take away.

## Rejected

| Alternative | Why not |
|---|---|
| Keep bash + curl + jq | 21–34 ms of process spawns per link, config sourcing, nothing warm between links |
| A video-specific tool | Video is the first use, not the only one; see D3 |
| TOML/YAML config plus a separate plugin system | Two mechanisms for one job; see D4 |
| Native plugins (Rust `cdylib`, C ABI, `dlopen`) | Breaks the single static binary (a statically linked musl binary can't `dlopen` at all), no stable Rust ABI, and a plugin crash takes down the daemon. Performance needs are met by core services instead |
| WebAssembly plugins (wasmtime/wasmer) | Language-agnostic and sandboxed, but adds a runtime of many MB to the binary and compile/instantiate cost at load, plus a host API to maintain in a second form. Reconsider if third-party plugins from untrusted sources become a goal |
| External-process plugins as the main mechanism (JSON over stdio, like LSP) | A process spawn or a resident process per plugin, and IPC on the hot path. Kept as a narrow escape hatch: a Lua plugin can run any program through `ctx.exec` |
| Python daemon | Interpreter start and memory for a resident process; no static binary |
| libcurl as the HTTP stack | 10 ms cold CPU cost vs 1.5–3 ms for rustls/reqwest, and a C TLS library in the static build; no network-side difference; see D8 |
| Embedding libmpv in the daemon | Saves the player's process start and IPC, but a daemon crash kills playback, window handling moves into the daemon, and the user's normal mpv config and scripts stop applying. Revisit only if measurements show player start dominates |
| mpv's ytdl hook as the video resolution path | See D5 |
| Pre-started or kept-alive idle player | 0–10 ms gain with OpenGL; a window-less idle player (the only kind that works on every compositor) was 20–90 ms slower (**MEASURED**) |
| Pooling googlevideo connections between clicks | Closed after 120 s idle; preconnecting per link gets the same effect (D9) |
| HTTP/2 PING to keep YouTube's connection | GOAWAY at 240 s regardless (**MEASURED**); see Q2 |
| Per-link client that does all the work (no daemon) | See D2; kept only as a debugging mode (`link-router open --no-daemon`) |
| LuaJIT | JIT speed is irrelevant for orchestration code; Lua 5.4 has integers and matches current Lua tooling. Revisit if Lua shows up in traces |
| link-router as the `mimeapps.list` default for http/https | Browsers' own default check fails, they prompt, and one "yes" rewrites the default (observed with Brave) |
| The same in `<desktop>-mimeapps.list` | Read first by xdg-mime and GIO, but browsers' checks read it too: Chromium re-prompts every start and its "set default" fails; Firefox's silently does nothing |
| Re-asserting the default from an inotify watcher | No live fight, but a prompt on every browser start |
| Shadowing `xdg-open` on `PATH` | Misses GIO and the portal: a link clicked in Telegram Desktop never called it (**MEASURED** with a logging shim); needs `/usr/local/bin` or a `PATH` order the session may not have |
| A GIO module hooking default-handler lookup | The `GDesktopAppInfoLookup` extension point is still registered but documented as unused; scheme defaults come from mimeapps lists only |
| `GIO_LAUNCH_DESKTOP` wrapper in the session environment | Undocumented test hook in glib; wraps every GIO launch without telling a default open from "Open with"; skipped for D-Bus-activatable apps; depends on environment propagation |
| A portal AppChooser backend with `always-ask` for http/https | Sees portal opens without touching browsers, but misses direct GIO, `xdg-open` and KIO; `always-ask` is global per content type and the backend must proxy file choosers. Kept as a possible add-on for the Firefox prompt gap (Q17) |
| `DBusActivatable` plus owning the application's bus name | Browser desktop IDs (`firefox`, `chromium`, `brave-browser`) aren't valid bus names; `firefox.desktop` sets `DBusActivatable=false` |
| Replacing or wrapping the desktop portal | Security-critical service with a single well-known name and no proxy mechanism |
| KDE `BrowserApplication`, KUriFilter plugins | `BrowserApplication` is only a fallback when no service claims http(s), and was removed in KIO 6.30; URI filters handle typed input only |
| Wrapping every installed browser's entry, checked only when a link arrives | A browser installed and made default later never sends a link through link-router, so nothing notices; the sentinel's inotify watch and start-time check replace it |

## Deviations from the consistency reviews

The design docs were checked for internal consistency twice (54 findings, then
45 after the interception design was added). All were applied as suggested
except:

- **Doctor wording** ("checks each route by which apps open links" in
  integration.md): superseded; the section was rewritten around interception
  (D12), and doctor now checks each opening path.
- **`single_instance` target option:** removed instead of defined; driver
  targets are always single-instance, and a second instance means a second
  target.
- **Global-name escape for plugins:** `@name` was chosen over `:name` because
  `:` already separates target names from event names.
- **Event callback arguments:** events pass one table with named fields plus
  `ctx` (`function(e, ctx)`) instead of positional arguments, so events can
  gain fields without breaking callbacks.
- **`pending Q5` in plugins.md:** left without bold, because it sits inside a
  Lua code block where Markdown emphasis doesn't render.
- **Schemes without a default handler:** dropped instead of specified (see D12,
  scope decisions).
- **mpv stream options:** `tcp_nodelay` became a `mpv-video` plugin default and
  `ca_file` a documented recommendation (D11), rather than both defaults or both
  recommendations.
- **Package removal:** no package pre-remove hook is specified, because shadows
  and state are per user; documentation tells users to run
  `link-router uninstall` first.

## Open questions

| # | Question | Settled by |
|---|---|---|
| Q1 | 0-RTT across daemon restarts or past a connection's lifetime: worth an OpenSSL build (~9 MB static, sessions persisted to disk) for ~58 ms on those requests? rustls 0-RTT against Google is unverified | a rustls 0-RTT test; trace data on how often the first YouTube request after 240 s idle happens |
| Q2 | Keeping YouTube warm past 240 s would take a real request every 4 minutes; worth it, or accept a cold request after idle? | trace data from real use; cost of periodic requests (rate limits) |
| Q3 | Deferring playback until the parallel audio track is added, without the bundled mpv script: mpv's manual doesn't document registering hooks over JSON IPC | a test against mpv's IPC; only matters without the proxy |
| Q4 | Per-link cost of the `mpv-video` plugin's Lua logic: decoding a ~35 KB player response into Lua tables and selecting formats in Lua, versus `json.select` extracting only the needed fields in Rust | benchmark once `lr-lua` exists |
| Q5 | Plugin API versioning: one integer `api` version checked at load, or semver per service | the first breaking change |
| Q6 | Probing unknown links costs a round trip; which routes get it, and whether it runs in parallel with a speculative handler | image/document plugin design |
| Q7 | Instagram carousels: first video only, or the carousel as a playlist | user preference |
| Q8 | A warm yt-dlp worker for `mpv-video.ytdlp` (keeps ~0.25 s of Python startup off that path) | trace data: how often links reach that resolver |
| Q9 | KWin adapter scope: window placement by KWin script at runtime, or only a shipped window rule | a Plasma machine to test on |
| Q10 | Link opener detection: the parent is often a launcher, a shell running `xdg-open`, `gio`, or the portal; walk up the process tree, or use the portal's app id | prototype |
| Q11 | One of five Instagram page fetches came back without media; cause and frequency | more samples, comparing headers and connection reuse |
| Q12 | `visionos` staying PO-token-free: yt-dlp records enforcement spreading on `android_vr` | watching yt-dlp releases; the fallback chain covers a break |
| Q13 | Request hedging delay (~0.6 s proposed: a lost SYN costs 1 s) | trace data on request time distribution |
| Q14 | Instagram CDN stream open: one direct request took 0.91 s to first byte; how it behaves through the proxy | measurement |
| Q15 | Shadow behaviour with running browsers: Chromium/Brave's check and Firefox/Zen's check with a per-launch `MOZ_APP_LAUNCHER` pass according to source; not yet tested with real browsers, including Firefox's crash-reporter restart through the router and the `userapp-*` path | a test with throwaway profiles in an isolated `$XDG_DATA_HOME`/`$XDG_CONFIG_HOME` |
| Q16 | Sentinel without a resident daemon: how often a default changes while no daemon runs, and whether `link-router sentinel` at session start plus the start-time check is enough | real use; setup offers a resident daemon as the alternative |
| Q17 | A portal AppChooser backend as an add-on that removes the Firefox/Zen prompt gap for portal opens | prototype, after Q15 |
| Q18 | Proposing a URL pre-dispatch hook upstream (MIME apps spec, GIO, KIO, xdg-desktop-portal), or the desktop-ID comparison in Firefox | discussion with upstream maintainers |
| Q19 | "Open with <default browser>" on an http(s) link is routed like a default open, because a shadow can't tell them apart. Acceptable, or should routing skip some openers (e.g. file managers, once opener detection (Q10) exists)? | user preference; Q10 |
