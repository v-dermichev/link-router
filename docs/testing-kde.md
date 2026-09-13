# Testing on KDE Plasma

Status: the KDE path is tested against KIO 6.29 (link interception) and a mock
of KWin's scripting API and D-Bus interface (window placement), not yet on a
real Plasma session. This checklist is for that first real run.

Needs Plasma 6, `mpv`, `curl`, and a browser set in System Settings > Default
Applications.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/install.sh | sh
```

Answer the questions (all sources is fine). At the end, `link-router doctor`
output is printed; the `https` line should say `[shadow] by-id link ok` and
`daemon: running`.

## Checks

Tick what matches; note what doesn't.

1. Click a YouTube link in a KDE app (Konsole: Ctrl+click, KMail, Kate, a
   Plasma widget). A video window opens in the bottom-right corner, above
   other windows, and plays.
2. Same from a non-KDE app (Telegram, Discord, Thunderbird, a Flatpak app).
3. While it plays, click a YouTube Short. The same window switches to a
   portrait size and stays in the corner.
4. Click a direct video link, e.g.
   `https://img-9gag-fun.9cache.com/photo/a9y3790_460svvp9.webm`. It plays in
   the same window, fitted into 960x720.
5. Switch to another virtual desktop (or focus another screen) and click a
   video link. The window moves to the current desktop and screen.
6. Drag the window somewhere else; it stays there until the next link.
   Make it fullscreen (double-click or `f`): it fills the screen, and a link
   clicked meanwhile plays fullscreen too. Leaving fullscreen puts it back in
   the corner.
7. Click an ordinary link (e.g. `https://kde.org`). It opens in the browser as
   usual.
8. Change the default browser in System Settings, then run
   `link-router doctor`: the new browser shows `[shadow]`.
9. Log out and in: `link-router doctor` still shows `daemon: running`.
10. Anything odd: a flash of the window somewhere else before it lands in the
    corner, the window hidden behind others, a window that doesn't resize.

## What to send back

```sh
plasmashell --version; echo "$XDG_SESSION_TYPE"
link-router version
link-router doctor
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.isScriptLoaded link-router
tail -n 60 ~/.local/state/link-router/daemon.log
cat ~/.local/state/link-router/mpv.log
journalctl --user -b --no-pager | grep -i 'link-router' | tail -n 40
```

The last line shows the KWin script's own messages ("placement script
active") and any script errors.

## How placement works there

mpv sizes its window from its `geometry` option, which the daemon sets before
each video. A KWin script, loaded once over D-Bus (`link-router kwin-script`
prints it), moves windows with the player's app ID (`link-router-mpv`) to the
bottom-right corner when they open and whenever their size changes, keeps
them above other windows, and moves them to the current desktop and screen
when a new video starts. Without the script (KWin unreachable), the window
still gets the right size; KWin decides where.

## Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/uninstall.sh | sh
```
