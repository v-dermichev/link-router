# Integration

Status: design.

## Interception

Status: design. The browser, GIO, portal and KIO behaviour below was verified
against source (Firefox main `0304f5a3e392`, Chromium main `c003bc18c18a`,
brave-core `e5995d7a3b29`, glib 2.90.0, xdg-desktop-portal 1.22.1, KIO/KService
6.30.0, xdg-utils 1.2.1) and with isolated `xdg-settings`/`xdg-mime`/GIO
queries; nothing was tested with a running browser yet (Q15).

### Why link-router is not the default handler

The obvious registration, link-router as the `mimeapps.list` default for
`x-scheme-handler/http(s)`, doesn't survive real use. Browsers check "am I the
default?" through the same lookup every app uses to open links, see "no", and
offer to fix it; a user who clicks "yes" sends every link straight to the
browser again (Brave did exactly that on the test machine: it rewrote
`x-scheme-handler/http`, `https`, `about`, `unknown` and `text/html`).
Desktop-specific lists (`hyprland-mimeapps.list`) and re-asserting the default
from a watcher only move the fight around. Shadowing `xdg-open` on `PATH` misses
GIO and the portal: a link clicked in Telegram Desktop never called it
(**MEASURED** with a logging shim). The alternatives and why they fail are
listed in [decisions.md](decisions.md#rejected).

So the handler that XDG resolves stays what it is, and link-router sits one step
later: in the desktop entry that every opening path runs.

### Intercepted schemes

`interception.schemes` (default `{ "http", "https" }`) plus every loaded
plugin's `schemes` form the intercepted set. For each scheme in the set, the
sentinel shadows the entry that is the default handler of
`x-scheme-handler/<scheme>`. A scheme without a default handler is not
intercepted, and doctor reports it; link-router never ships or registers an
entry of its own for any scheme (see
[decisions.md](decisions.md#d12-intercept-at-the-default-handlers-desktop-entry-kept-intact-by-a-sentinel)).

Only links whose scheme is in the set are routed. Anything else that reaches a
shadow (a `file://` URI or a plain path from "Open with" on a local file, a
scheme outside the set) goes straight to the original entry from the client,
without the daemon.

### Shadow entries

Every path an application uses to open a link resolves the default desktop ID
to a file and runs that file's `Exec`: `xdg-open` (generic mode), GIO
(`g_app_info_launch_default_for_uri`), the desktop portal's OpenURI (which
launches by desktop ID), and KIO on Plasma. Desktop files are looked up in
`$XDG_DATA_HOME/applications` before `$XDG_DATA_DIRS`, so a file with the same
ID there wins. link-router writes such a **shadow**:

- **File:** `$XDG_DATA_HOME/applications/<id>`, where `<id>` is the desktop ID
  including `.desktop`. An ID that comes from a subdirectory
  (`kde4/foo.desktop` → `kde4-foo.desktop`) is written under its flat ID.
- **Content:** a copy of the effective original entry, with the main `Exec` in
  `[Desktop Entry]` replaced by `Exec=<by-id path> %U`, where the by-id path is
  `$XDG_DATA_HOME/link-router/by-id/<id>`, written as an absolute path.
  `by-id/<id>` is a symlink to the install path recorded in the state file; the
  binary learns from its `argv[0]` which entry it shadows. The path must be
  distinct per ID: Firefox's "set as default" takes the first entry (in
  hash-table order, effectively arbitrary) whose `Exec` binary equals its own
  path, so shadows sharing one path would let it pick another browser's ID.
- **Desktop actions** (new window, private window) get
  `Exec=<by-id path> <the action's original arguments>` when the action runs
  the same executable as the main `Exec`, so they go through the
  non-link-arguments branch below (and get `MOZ_APP_LAUNCHER` for Gecko).
  Actions that run another executable stay unchanged.
- **`TryExec`:** kept when the original has one. When it doesn't, the shadow
  gets `TryExec=<by-id path>`, so GIO and KService skip the shadow if the
  link-router binary is gone. The shadow's `Exec` binary and `TryExec` must
  exist: if either is missing, GIO and the portal silently fall through to the
  system entry with the same ID, while `xdg-settings` still reports the ID as
  default.
- **`DBusActivatable=false`:** GIO and the portal otherwise activate the
  original over D-Bus instead of running `Exec` (KIO never activates when
  opening URLs).
- **Never `Hidden=true`:** the portal then drops the default and shows its app
  chooser.
- Name, icon, `StartupWMClass` and `MimeType` stay as they are.
- **Marker keys:** `X-Link-Router-Shadow=1`, `X-Link-Router-Source=<path>`,
  `X-Link-Router-Source-Hash=<sha256>`. The state file also records the hash of
  the shadow file as written. Files without the marker are never treated as
  link-router's.
- **Originals:** an original from `$XDG_DATA_DIRS` is read live from
  `X-Link-Router-Source` when it is launched, so a package update takes effect
  at once. An original that was the user's own file in
  `$XDG_DATA_HOME/applications` (a hand-written override, or a browser-generated
  `userapp-*` entry) is moved to `$XDG_DATA_HOME/link-router/originals/<id>`,
  because the shadow takes its path.

Shadowed IDs:

- the current default handler of every intercepted scheme;
- for a Gecko default (Firefox, Zen, other Gecko browsers), every other entry
  that runs the same Gecko binary: its `Exec` executable, after skipping `env`
  assignments, `flatpak run` and `/snap/bin` wrappers, resolves to the same
  binary, it lists an `x-scheme-handler/http` or `https` MimeType, and its
  arguments carry no profile or app selection (`-P`, `--profile`, `--kiosk`,
  `--app-id` and similar). This keeps every ordinary launch of that browser
  going through link-router, so its default check sees the shadow (see the
  table below). Chromium-family browsers compare desktop IDs and need no such
  shadows; their PWA and shortcut entries are never shadowed.

A shadow whose ID no longer qualifies is removed, and a moved original is put
back. Choosing an unrelated browser explicitly ("Open with") is not routed.

What the browser sees:

| Browser | Default check | With a shadow |
|---|---|---|
| Chromium, Chrome | `xdg-settings check default-web-browser <own desktop id>` compares desktop IDs (http, text/html, https); a failed check counts as unknown and doesn't prompt | passes: the default is still its own ID (verified with a same-ID file in an isolated `$XDG_DATA_HOME`) |
| Brave | the same check with `brave-browser.desktop`; on anything but "yes" it runs `xdg-settings get default-web-browser` and prompts unless the output contains `brave-browser` | passes, as above |
| Firefox, Zen (native package) | only http and https: the GIO default's `Exec`, `argv[0]` looked up in `PATH` (an absolute path is kept as is, no symlink resolution) and string-compared with `$MOZ_APP_LAUNCHER` if set, else with the resolved real binary (`/proc/self/exe`); the desktop ID isn't compared | passes when the running instance has `MOZ_APP_LAUNCHER` equal to the by-id path of the current http/https default, which link-router sets on every launch of a Gecko original (see below). Started any other way (terminal, compositor keybind, autostart), it sees "not default" and prompts |
| Firefox under Snap | `xdg-settings check default-web-browser firefox.desktop`, like Chromium | passes, as for Chromium |
| Firefox under Flatpak | GIO lookups go through the portal and never match | always "not default"; its JS skips the prompt |

Firefox's "set as default":

- **Started through link-router** (`MOZ_APP_LAUNCHER` set): it finds the shadow
  whose `Exec` is that by-id path and sets it as default again. Nothing changes
  except the extra types it always writes (`x-scheme-handler/chrome`,
  `application/xhtml+xml`, `htm`/`html`/`shtml`/`xhtml`/`xht` extension types).
- **Started outside its entries:** no entry's `Exec` matches the real binary, so
  it creates `userapp-<Brand>-XXXXXX.desktop` (NoDisplay) in
  `$XDG_DATA_HOME/applications` and makes it the default for http, https,
  chrome, text/html and xhtml. The sentinel recognises such files (name
  `userapp-*-*.desktop`, `NoDisplay=true`, Gecko executable) as
  browser-generated rather than the user's, so they follow the repair policy
  instead of always asking; it shadows the new ID, and routing continues. The
  previous `userapp-*` shadow no longer qualifies once it isn't the default, so
  it is removed and its original put back; the next "set as default" from
  outside finds that restored file and reuses it instead of creating another.
- It writes only `$XDG_CONFIG_HOME/mimeapps.list`; an existing
  `<desktop>-mimeapps.list` in the same directory silently overrides it, which
  the sentinel's resolution accounts for.

Rules kept from `open-url`:

- **`Exec` paths are absolute and never quoted.** Outside the desktops
  xdg-open has its own branch for, `xdg-open` takes the first
  whitespace-separated word of `Exec` literally, quotes included, and falls back
  to `$BROWSER` and then a hard-coded browser list when that isn't a command.
  Every writer of shadows (setup, `enable`, the sentinel) refuses a data
  directory whose path contains whitespace, quoting characters or `%`.
- link-router ships no desktop entry that claims any intercepted scheme (its own
  menu entry for setup has no `MimeType`), so a browser's "set as default" can't
  pick it.

### Opening through a shadow

The client started through `by-id/<id>` decides in this order:

1. **`argv[0]` → `<id>`**, the link's fallback entry.
2. **No arguments** (the app launched from a menu or dock): run the original's
   main command, without the daemon.
3. **Arguments that aren't all URLs** (`--new-window`, `-P profile`, a desktop
   action's arguments, or the original argv Firefox's crash reporter passes
   when it restarts through `MOZ_APP_LAUNCHER`): run the original's executable
   with those arguments unchanged, without the daemon.
4. **Any URL whose scheme isn't intercepted, or any `file://` URI:** run the
   original with all the arguments, without the daemon.
5. **No config, or interception not enabled in the state file:** run the
   original with the URLs, without the daemon.
6. **Otherwise:** hand the URLs to the daemon with `<id>` as their fallback.

Branches 2–5 `exec` the original in place (keeping the PID and startup
notification) through the [desktop launcher](#desktop-launcher) rules. The
client sets `MOZ_APP_LAUNCHER` for Gecko originals in these branches too.

Each link falls back to the original of the entry it came through. For `http`
and `https` links, `router.config({ browser = … })` overrides that fallback;
links of other schemes always fall back to their own original.

`MOZ_APP_LAUNCHER`, for a Gecko original: the by-id path of the current http or
https default whose original runs the same Gecko binary; if there is none, the
by-id path of the entry being launched. Its only other uses in Firefox are the
X11 session-manager restart command and the crash reporter's restart; Firefox
unsets it for handlers it launches itself, but other child processes inherit
it.

### State

`$XDG_STATE_HOME/link-router/state.json`:

| Field | Meaning |
|---|---|
| `enabled` | interception consent; nothing is shadowed unless `true` |
| `install_path` | absolute path of the link-router binary that `by-id` symlinks point at |
| `autostart` | autostart entries link-router added (paths), for uninstall |
| `shadows` | per ID: source path, source hash, hash of the shadow as written, whether the original was moved |
| `sentinel` | time and result of the last pass, repairs done, a pending "ask" |
| `gecko` | per Gecko binary: the `MOZ_APP_LAUNCHER` value of its last launch, for doctor |

### Sentinel

The sentinel keeps interception intact. It does nothing unless `enabled` is
`true`. It runs the same check-and-repair pass:

- whenever the daemon starts (on demand from a click, or from autostart);
- on config reload;
- while the daemon runs, on inotify events for the directories that hold
  mimeapps lists and desktop entries: `$XDG_CONFIG_HOME`, `$XDG_CONFIG_DIRS`,
  `$XDG_DATA_HOME/applications`, `$XDG_DATA_DIRS/applications` (Flatpak export
  directories included), and their parents, so directories created later are
  picked up. Watching directories rather than files survives the atomic
  renames GLib, Firefox and xdg-utils use. Events for files the sentinel wrote
  itself (matched by the write generation it records) are ignored, and a pass
  starts after 500 ms without further events, so a browser writing several
  types in a row is seen in its final state;
- on `link-router sentinel`, meant for session autostart: it runs the full
  config (including `router.use`, for plugins' schemes and the policy), checks,
  and repairs per policy. With `repair = "ask"` and something to ask, it starts
  the daemon (or hands the question to a running one) instead of exiting with a
  question nobody can answer.

All resolution uses the canonical session environment: `XDG_CURRENT_DESKTOP`
(a colon-separated list), `XDG_DATA_HOME`, `XDG_DATA_DIRS`, `XDG_CONFIG_HOME`
and `XDG_CONFIG_DIRS` read from a session process (the compositor, or the
desktop portal) through `/proc`, not from whatever process started the daemon.
When that environment can't be read, or a resolution disagrees with it, the
sentinel repairs missing shadows but never removes one.

Checks:

1. Resolve the default ID for each intercepted scheme the way GIO and xdg-mime
   do: directories in order (`$XDG_CONFIG_HOME`, `$XDG_CONFIG_DIRS`,
   `$XDG_DATA_HOME/applications`, `$XDG_DATA_DIRS/applications`), and within
   each directory `<desktop>-mimeapps.list` for every desktop in
   `XDG_CURRENT_DESKTOP`, in order, before `mimeapps.list`.
2. Every ID that qualifies (see Shadowed IDs) has a marked shadow; its source
   still exists and the source hash matches (a package update changes the
   original).
3. A marked shadow whose content no longer matches the hash recorded when it
   was written was edited by someone else: the sentinel asks whatever the
   policy, and stops managing that file until answered.
4. No marked shadow exists for an ID that no longer qualifies; a default whose
   source was uninstalled no longer qualifies.
5. Every `by-id` symlink exists and points at `install_path`, which exists
   (checked with `stat`; the running process's `/proc/self/exe` is never used,
   because it can be a deleted or different file after an update), and every
   shadow's `TryExec` exists.
6. On Plasma, the KService cache is refreshed after changes (`kbuildsycoca6`)
   when no `kded6` process runs (checked in `/proc`).

Repair policy, `router.config({ interception = { repair = … } })`:

| Value | Behaviour |
|---|---|
| `"auto"` | Repair silently and log it |
| `"ask"` | Desktop notification with "Repair" / "Ignore" actions; without an action-capable notification server, the notification says to run `link-router doctor --fix`. A pending question keeps the daemon from exiting on idle |
| `"report"` | Notification and log only |

Replacing a user's own unmarked file (a hand-written `brave-browser.desktop`)
and overwriting an edited shadow (check 3) always ask, whatever the policy.
Browser-generated `userapp-*` entries follow the policy.

**Gap:** a default changed while no daemon runs is noticed at the next daemon
start or `link-router sentinel`. Until then, clicks go straight to the new
handler and never start the daemon. Closing it takes a resident daemon
(`daemon.idle_exit = false`) or `link-router sentinel` in session autostart;
setup offers both (**pending** Q16 on whether that is enough).

### First run, enable and uninstall

`link-router setup` is a terminal wizard (a later milestone). It shows the
entries it would shadow, asks for consent, the repair policy, which plugins to
enable, and whether to keep the daemon resident or add `link-router sentinel`
to session autostart; then it writes `init.lua`, sets `enabled` and runs the
sentinel. Declining consent writes nothing.

Until the wizard exists, `link-router enable` records the install path, sets
`enabled = true` and runs a sentinel pass; without it, nothing is intercepted.

`link-router uninstall`:

1. stops a running daemon;
2. removes the autostart entries recorded in the state file;
3. sets `enabled = false`, so nothing re-creates shadows;
4. for each recorded shadow: if the path still holds the shadow exactly as
   written, removes it and puts a moved original back; otherwise leaves the
   file alone and reports it;
5. removes `by-id`.

`mimeapps.list` is never edited, so nothing there needs restoring. Removing the
binary without `uninstall` leaves shadows whose `TryExec` or `Exec` is missing:
GIO, the portal and KService then fall through to the original entry, but
`xdg-open`'s generic mode goes to its fallback browser list. Shadows and state
are per user, so a package pre-remove hook can't clean them up; package
documentation should tell users to run `link-router uninstall` first.

### `link-router doctor`

Checks every opening path without opening anything. Doctor itself doesn't call
D-Bus; the daemon row reports what the running daemon found.

| Path | Check |
|---|---|
| `xdg-mime query default x-scheme-handler/<scheme>` for each intercepted scheme | returns an ID whose effective desktop file is a marked shadow; a scheme without a default handler is reported as not intercepted |
| `xdg-open` generic mode | the first word of that file's `Exec`, as `xdg-open` parses it, is the by-id path |
| GIO | `gio mime` default and the file it resolves are the same shadow |
| desktop portal | the same resolution evaluated with the portal process's environment (`XDG_CURRENT_DESKTOP`, `XDG_DATA_DIRS`, `XDG_CONFIG_*` read from `/proc`) |
| KIO (Plasma) | KService resolves the https handler to the shadow |
| browser checks | `xdg-settings check default-web-browser <id>` answers `yes`; for Gecko originals, the `MOZ_APP_LAUNCHER` of the last launch (state file) |
| `$BROWSER` (warning only) | in the canonical session environment: unset, or pointing at link-router; programs that honour it skip desktop entries |
| state | `enabled`, `install_path` exists, pending questions |
| daemon | socket reachable, protocol version, config and plugins load, every target's program found, compositor adapter connected (as reported by the daemon), sentinel policy and last result |

`link-router doctor --fix` runs the sentinel's repairs regardless of the
policy, asking before replacing an unmarked user file or an edited shadow.

## App drivers

A target with `driver = "…"` is a managed app: the driver keeps an instance
running, reuses it for the next item, holds an IPC connection to it, and turns
the app's own events into target events (see
[lua-api.md](lua-api.md#events-routeronname-fn)). Drivers are Rust, in
`lr-drivers`, behind one trait:

```rust
#[async_trait]
trait Driver: Send + Sync {
    /// Start the app if it isn't running. Called by the core when a
    /// resolve+target chain starts (route `prepare`), or by `ctx.target(name):prepare()`.
    async fn prepare(&self, env: &LinkEnv) -> Result<()>;
    /// Open content (or a content list) in the running instance, starting it if needed.
    async fn open(&self, content: &ContentOrList, opts: &OpenOpts) -> Result<ItemId>;
    /// Raw app-specific command, exposed to Lua as `target:command(args)`.
    async fn command(&self, args: serde_json::Value) -> Result<serde_json::Value>;
    /// Events: Started, ContentStarted, ContentShown, ContentEnded { reason },
    /// Idle, Custom { name, data }.
    fn events(&self) -> broadcast::Receiver<DriverEvent>;
}
```

`OpenOpts` combine the route's or target's `open_opts` with the `opts` of an
`act.open`/`act.resolve` call (the call wins), including `mode = "replace" |
"append"`.

**Targets without a driver** (`exec`, `desktop`) start a process per item. The
core keeps its handle: a non-zero exit within the target's `failure_grace`
(default 2 s) is a target failure and runs the route's fallback. A temporary
file from `core.download` is removed at the later of the process's exit and
`keep` after the launch (default `"10m"`): single-instance and D-Bus-activated
apps hand the file to a running instance and exit at once, so the launcher's
exit alone can't decide the file's lifetime.

**Lifecycle rules shared by all drivers:**

- An instance is never quit while the fallback engine still owns a link whose
  item it was showing, so a retry reuses the instance instead of starting a
  new one.
- Start-time options (`app_id`, `args`, `env`, the socket path) apply when an
  instance is next started; a config reload doesn't restart running
  instances. An instance whose target was removed or renamed by a reload is
  quit when it next goes idle.
- If a link's chain ends in the fallback and the instance started for it by
  `prepare()` never received an item, the driver quits that instance.

Drivers planned:

| Driver | App | IPC | Status |
|---|---|---|---|
| `mpv` | mpv | JSON IPC socket | designed below |
| `imv` | imv | `imv-msg` socket protocol | idea (would serve an `image` plugin) |

### mpv

The driver owns the player process and one persistent connection to its IPC
socket (`$XDG_RUNTIME_DIR/link-router/<target>.sock`), and loads one bundled
mpv script, `link-router-mpv.lua` (installed next to the binary's data, not in
the user's `scripts/`).

#### Target options

| Option | Default | Meaning |
|---|---|---|
| `app_id` | `"link-router-mpv"` | `--wayland-app-id`, what compositor rules match |
| `args` | `{}` | extra mpv arguments for a new instance |
| `env` | `{}` | extra environment for a new instance; a `false` value unsets the variable |
| `network_timeout` | `"60s"` (mpv's default) | per-file `network-timeout` for items |
| `proxy` | `true` | open streams through the core's streaming proxy when it is available |
| `stream_options` | `{}` | extra `--stream-lavf-o` values, used when an item is loaded without the proxy |
| `ca_file` | `nil` | `--tls-ca-file`, used when an item is loaded without the proxy |

Per-item options (`open_opts`, `act.open` `opts`): `title`, `mode`,
`network_timeout`, and mpv properties to set for the item (`properties = { … }`,
e.g. cache settings).

#### Start

```
mpv --idle=yes --force-window=immediate --ytdl=no
    --input-ipc-server=<sock> --wayland-app-id=<app_id>
    --script=<data dir>/link-router-mpv.lua
    <args…>
```

- `prepare()` starts it when the chain starts, in parallel with the resolver's
  network request. `--force-window=immediate` creates the window at start:
  an idle player without a window was 20–90 ms slower to first frame
  (**MEASURED**). The cost is a briefly empty window for links whose chain
  ends in the fallback; the driver quits that instance.
- `--idle=yes`: the driver decides when the player closes (see Events).
- `--ytdl=no`: mpv never resolves links itself. With the ytdl hook loaded, a
  stream URL that fails to open first goes through `yt-dlp -J` before mpv
  reports the failure (`end-file` at 947–975 ms instead of 182–183 ms in a local
  test, **MEASURED**).

#### Opening content

For each item, over the persistent connection:

1. `set_property` for the item's `properties` that differ from the instance's
   current values.
2. **With the proxy:** register the video URL and the `audio` extra with the
   proxy, then `["loadfile", "edl://!new_stream;%<n>%<proxy video url>;!new_stream;%<n>%<proxy audio url>", "replace", -1, { … }]`.
   The proxy has already started both fetches, so the EDL's sequential opens are
   localhost connects with data waiting: first frame 66 ms after `loadfile` on
   warm connections, 151 ms on new ones (**MEASURED**, vo=null).
3. **Without the proxy:** `set_property user-data/link-router/audio <audio url>`,
   then `["loadfile", <video url>, "replace", -1, { … }]`. The bundled script, at
   `start-file`, reads the audio URL with `get_property_native` (the string
   form is JSON-quoted) and runs `audio-add <url> select` asynchronously; an
   `on_preloaded` hook defers until the track is added (or 1.5 s pass), so
   video and audio start together instead of the audio's first 0.1–0.3 s being
   skipped when it opens later (**MEASURED**). First frame 0.21–0.25 s after
   `loadfile` on real streams with a window (**MEASURED**; 211–218 ms in the
   proxy comparison, which ran with `--vo=null`). A failed `audio-add` is reported to the driver
   with `script-message link-router audio-failed`, because a failed external
   track produces no `end-file`.

Per-file options go in the `loadfile` JSON object (`force-media-title`,
`http-header-fields`, `network-timeout`): set globally, `force-media-title`
persists into later files (**MEASURED**). mpv applies per-file options after
`start-file` fires, so the script reads item data from `user-data`, not from
per-file options, at `start-file`.

`audio-add` issued over IPC in the same batch as `loadfile` is cancelled by the
file change, which is why the script issues it. Registering the deferring hook
over IPC instead of from a script is not documented in mpv's manual
(**pending** Q3).

#### Events

| mpv | Target event |
|---|---|
| process up, IPC connected | `started` |
| `start-file` for the item | `content_started` |
| first `playback-restart` after `start-file` with `paused-for-cache` false | `content_shown`: the item is playing, not just showing a first frame; the link's trace ends here |
| `video-params` change (`dw`, `dh` known) | `video_params` with `target`, `content`, `link`, `w`, `h`, `dw`, `dh` |
| `end-file` reason `error`, or `audio-failed` | `content_ended` with `"error"`; before `content_shown` the chain re-enters at the next resolver outside the producer's families |
| `end-file` reason `eof`/`stop` | `content_ended` with that reason |
| `idle-active` true and no link owning an item of this instance is still being handled | `idle`; unless a callback returns `false`, the driver sends `quit` |
| IPC connection closed | instance forgotten; the next item starts a new one |

mpv logs `first video frame after restart shown` before the initial cache
pause, so a first-frame event would hide buffering (**MEASURED**: first frame
at 197 ms, then 0.44 s paused at 150 KB/s).

## Desktop launcher

One launcher runs desktop entries for every caller: link fallbacks, the client's
passthrough branches, `router.act.browser`, `open = { desktop = … }` routes and
`desktop` targets.

- **Resolve** `<id>` (with or without `.desktop`) in
  `$XDG_DATA_HOME/applications`, then each of `$XDG_DATA_DIRS/applications`,
  using the canonical session environment ([Sentinel](#sentinel)).
- **De-shadow:** if the resolved file is a marked shadow, use its original: the
  moved copy in `originals/<id>`, else the live file named by
  `X-Link-Router-Source`. The launcher never runs a file whose command is
  link-router or a by-id path.
- **Run:** parse `Exec` and expand field codes: `%u`/`%U` with the URLs;
  `%f`/`%F` with the local path of `file://` content (other URLs are rejected
  for entries that only take files); `%i`, `%c`, `%k` per spec; drop the rest.
  No shell. Honour `Path` and `Terminal`.
- **D-Bus-activatable originals** (`DBusActivatable=true`): call
  `org.freedesktop.Application.Open` with the URLs, or `Activate` when there are
  none.
- **Gecko originals** (detected generically, e.g. `application.ini` next to the
  executable): set `MOZ_APP_LAUNCHER` as described in
  [Opening through a shadow](#opening-through-a-shadow).
- **Environment:** the canonical session environment, plus
  `XDG_ACTIVATION_TOKEN` (Wayland) and `DESKTOP_STARTUP_ID` (X11) from the
  client message, so the app can take focus.
- **Tracking:** the launched process is tracked like an `exec` target (a
  non-zero exit within the grace period is a failure).

**Loop guard.** The daemon remembers each URL it handed to an original or a
target for 5 s. The same URL arriving again within that window, by any path
(an `exec` target calling `xdg-open`, a browser handing an unsupported scheme
back to the system handler), goes straight to its fallback original without
routing, and the trace records the loop.

## Compositor adapters

Adapters are optional. With no adapter (`ctx.compositor.name == "none"`),
windows are placed by the compositor's own rules.

| Capability | Hyprland | KWin | none |
|---|---|---|---|
| Detect | `HYPRLAND_INSTANCE_SIGNATURE` | `KDE_SESSION_VERSION` + D-Bus `org.kde.KWin` (needs `DBUS_SESSION_BUS_ADDRESS`) | fallback |
| Find a window | clients list: `pid` (from `{ target = … }` or `{ pid = … }`), class (`{ app_id = … }`) or `{ address = … }` | KWin script `workspace.windowList()` by pid or resource class | — |
| Resize and move | Lua dispatch `hl.dsp.window.resize` / `move` | KWin script setting `frameGeometry` | — |
| Focused workspace | monitors list (special workspace first) | current desktop | — |
| Move to workspace | Lua dispatch `hl.dsp.window.move({ workspace = … })` | KWin script `window.desktops` | — |
| Transport | Hyprland request socket `$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock` directly, several requests batched in one connection, no `hyprctl` process | D-Bus `org.kde.kwin.Scripting` | — |

Notes:

- `fit_window(selector, { content_size, box, anchor, margin })` is one service
  call: the adapter fits `content_size`'s aspect ratio inside `box`, computes
  the anchor corner on the monitor the window is on (monitor offset + logical
  size), then resizes and moves in one batched request. When a resolver
  already knows the video size (`content.meta.width/height`), the `mpv-video`
  plugin can call it before the first frame instead of waiting for
  `video_params`.
- `link.workspace` records the focused workspace when the link was received;
  moving the window there on the first `video_params` puts it where the click
  happened even if focus moved while the link resolved.
- On Hyprland, floating windows keep their rule size and ignore the client's
  own resizes (measured in `open-url`), so fitting needs the adapter. Where
  the compositor honours client resizes, mpv's `--autofit` gives the same
  shapes without an adapter.
- A rule's `move` plus a later resize can push a window past the monitor edge;
  `fit_window` always computes the position from the new size.
- The adapter reconnects when the client message carries a different
  `HYPRLAND_INSTANCE_SIGNATURE` than the one it's connected to (the compositor
  restarted).
- The KWin adapter is designed but untested; there is no Plasma machine yet
  (**pending** Q9).
- A compositor window rule is still the place for float/stacking policy; the
  repository ships `examples/hyprland-rule.lua` rather than creating rules at
  runtime.
