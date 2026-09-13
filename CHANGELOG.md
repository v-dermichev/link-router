# Changelog

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
