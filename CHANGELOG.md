# Changelog

## 0.1.0-beta.4

- `install.sh` upgrades an existing installation: it finds it through the
  recorded install path, skips the download when the version is current,
  asks once, keeps the config and interception, refreshes shadow entries with
  the sentinel and restarts the registered service (systemd, OpenRC or
  autostart). `--reinstall` runs the full installation.

## 0.1.0-beta.3

- KDE: fullscreen and maximized players are no longer pushed back into the
  corner by the placement script (the fullscreen window ended up shifted by
  the margins).
- A running player that is fullscreen keeps its size and place when the next
  link loads, on every compositor.
- sway: a `for_window` rule on the new player's PID floats, sizes and places
  it before it maps; a running player is moved with criteria commands and
  follows the focused workspace. The player is borderless on sway.
- FreeBSD 15: release binary, inotify support, the installer picks the
  FreeBSD build and installs missing packages with `pkg`.

## 0.1.0-beta.2

- KDE Plasma 6: a KWin script, loaded over D-Bus once per daemon (reloaded
  after a KWin restart), anchors the player bottom-right, keeps it above other
  windows and moves it to the current desktop and screen for each new video.
  `link-router kwin-script` prints it. Untested on a real Plasma session yet.
- KDE's `BrowserApplication` counts as the default browser when no http(s)
  scheme handler is set (KIO uses it only then); `doctor` warns about the
  `!command` form, which can't be intercepted.
- Without Hyprland, a new player now gets the fitted size also when the video
  size is only known once the stream opens (direct links); before, the window
  opened at the video's own size. mpv gets `--geometry=WxH-35-25` and
  `--x11-name`, so X11 window managers place it bottom-right.
- `link-router default-handler SCHEME`; the installer uses it (with the
  downloaded binary, before installing) to report the default browser the way
  link-router resolves it.
- `install.sh` asks which links play in mpv on every interactive run, also when
  a config already exists, and flags apply to existing configs. The choice
  lives in a marked `router.use("mpv-video", { sites = … })` block at the end
  of `init.lua`, replaced on each change; the rest of the config is untouched.

## 0.1.0-beta

First release: the MVP of the design in `docs/`.

- Interception at the default browser's desktop entry: `enable`, `disable`,
  `sentinel`, `doctor`, `stop`, `version`. Shadow entries follow default-browser
  changes while the daemon runs; customised entries are taken over only with
  consent and restored by `disable`.
- Client/daemon split: links and the clicking app's environment reach the
  daemon over a Unix socket in about 1 ms; everything the daemon doesn't play
  opens in the original handler. `daemon --resident` for service managers.
- `mpv-video` plugin: YouTube and Instagram resolved directly, yt-dlp fallback,
  direct media links (`.mp4`, `.webm`, ...), one reused mpv, window size and
  position set before the first frame (Hyprland runtime rule, `--geometry`
  elsewhere).
- Lua config: `router.config` (`daemon.idle_exit`) and `router.use("mpv-video", …)`.
- `install.sh`: per-source choice (`--youtube`, `--instagram`, `--direct`,
  `--all`), requirements check, optional package installation, checksum
  verified download of the static binary, default config, enable, and a login
  service (systemd user unit, OpenRC user service, or XDG autostart).
- `uninstall.sh`: removes the service, restores browser entries, removes the
  binary, data and logs; `--purge` also removes the config.
