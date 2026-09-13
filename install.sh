#!/bin/sh
# link-router installer.
#
#   curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/install.sh | sh -s -- --yes
#
# Run with --help for options. The binary, config and data go into your home
# directory; root (sudo or doas) is used only to install missing packages, after asking.

set -eu

REPO="v-dermichev/link-router"
DEFAULT_VERSION="0.1.0-beta"
ASSET="link-router-x86_64-unknown-linux-musl"

usage() {
    cat <<EOF
Usage: install.sh [options]

  --yes            don't ask: accept defaults and let link-router take over a
                   browser desktop entry you customised (restored by 'disable')
  --no-enable      install only; run 'link-router enable' yourself later
  --youtube        play YouTube links (videos, shorts, live, embeds) in mpv
  --instagram      play Instagram reels and video posts in mpv
  --direct         play direct media links (.mp4, .webm, .mkv, .mov, .m3u8, ...) in mpv
  --all            all of the above; without any of these flags the installer
                   asks per source (all of them when it can't ask)
  --no-config      don't write ~/.config/link-router/init.lua (links then pass
                   straight through to the browser)
  --no-deps        never install missing packages (mpv, yt-dlp), only report them
  --service KIND   how the daemon runs at login: auto (default), systemd, openrc,
                   autostart (XDG autostart entry) or none (started by the first
                   click, exits when idle; the sentinel then can't repair
                   interception while it isn't running)
  --version V      release to install (default $DEFAULT_VERSION; 'latest' for the
                   newest non-prerelease)
  --binary PATH    install this local binary instead of downloading one
  --bin-dir DIR    where to put the binary (default ~/.local/bin)
  -h, --help       show this help

Environment: LINK_ROUTER_VERSION, LINK_ROUTER_BIN_DIR, LINK_ROUTER_BASE_URL
(download from this URL instead of GitHub releases; it must serve
$ASSET and $ASSET.sha256).

Uninstall with uninstall.sh from the same place.
EOF
}

say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

# Piped into sh, stdin is the script itself: questions go to the terminal.
tty_ok() { [ -r /dev/tty ] && [ -w /dev/tty ] && (: </dev/tty >/dev/tty) 2>/dev/null; }

# ask QUESTION DEFAULT(y|n) -> status 0 for yes
ask() {
    if [ "$ASSUME_YES" = 1 ]; then return 0; fi
    if ! tty_ok; then [ "$2" = y ]; return; fi
    if [ "$2" = y ]; then hint="[Y/n]"; else hint="[y/N]"; fi
    printf '%s %s ' "$1" "$hint" >/dev/tty
    read -r answer </dev/tty || answer=""
    case "$answer" in
        [Yy]*) return 0 ;;
        [Nn]*) return 1 ;;
        *) [ "$2" = y ] ;;
    esac
}

# Package manager command for installing packages, "" when unknown.
PKG=""
detect_package_manager() {
    if has pacman; then PKG=pacman
    elif has apt-get; then PKG=apt
    elif has dnf; then PKG=dnf
    elif has zypper; then PKG=zypper
    elif has xbps-install; then PKG=xbps
    elif has apk; then PKG=apk
    fi
}

pkg_command() {
    case "$PKG" in
        pacman) printf 'pacman -S --needed' ;;
        apt) printf 'apt-get install' ;;
        dnf) printf 'dnf install' ;;
        zypper) printf 'zypper install' ;;
        xbps) printf 'xbps-install -S' ;;
        apk) printf 'apk add' ;;
    esac
}

# install_packages PKG... ; runs the package manager as root, answering its
# questions on the terminal (stdin may be this script).
install_packages() {
    cmd=$(pkg_command)
    if [ "$ASSUME_YES" = 1 ] || ! tty_ok; then
        case "$PKG" in
            pacman) cmd="$cmd --noconfirm" ;;
            apt | dnf | xbps) cmd="$cmd -y" ;;
            zypper) cmd="zypper --non-interactive install" ;;
        esac
    fi
    if [ "$(id -u)" = 0 ]; then root=""
    elif has sudo; then root="sudo"
    elif has doas; then root="doas"
    else warn "neither sudo nor doas found; run as root: $cmd $*"; return 1
    fi
    say "  running: ${root:+$root }$cmd $*"
    if tty_ok; then
        # shellcheck disable=SC2086
        $root $cmd "$@" </dev/tty
    else
        # shellcheck disable=SC2086
        $root $cmd "$@" </dev/null
    fi
}

# offer_package COMMAND PACKAGE REQUIRED(1|0) WHY
offer_package() {
    if has "$1"; then return 0; fi
    if [ "$NO_DEPS" = 1 ] || [ -z "$PKG" ]; then return 1; fi
    if [ "$3" = 1 ]; then
        ask "$1 is missing ($4). Install it with $(pkg_command) $2?" y || return 1
    else
        # Optional packages are never installed without asking, even with --yes.
        [ "$ASSUME_YES" = 0 ] && tty_ok || return 1
        ask "$1 is missing ($4). Install it with $(pkg_command) $2?" n || return 1
    fi
    install_packages "$2" || { warn "installing $2 failed"; return 1; }
    has "$1"
}

unsafe_path() {
    case "$1" in
        *[[:space:]\"\'\\\$\`%]*) return 0 ;;
    esac
    return 1
}

sha256_of() {
    if has sha256sum; then sha256sum "$1" | cut -d' ' -f1
    elif has shasum; then shasum -a 256 "$1" | cut -d' ' -f1
    else return 1
    fi
}

fetch() {
    if has curl; then curl -fsSL --proto '=https,http' --retry 2 -o "$2" "$1"
    else wget -q -O "$2" "$1"
    fi
}

# Desktop file for an ID, searched like the XDG spec does.
find_desktop_file() {
    old_ifs=$IFS
    IFS=:
    for dir in "$DATA_HOME" ${XDG_DATA_DIRS:-/usr/local/share:/usr/share}; do
        if [ -f "$dir/applications/$1" ]; then
            IFS=$old_ifs
            printf '%s\n' "$dir/applications/$1"
            return 0
        fi
    done
    IFS=$old_ifs
    return 1
}

check_requirements() {
    step "Checking requirements"
    errors=0

    [ "$(uname -s)" = Linux ] || { warn "link-router supports Linux only (found $(uname -s))"; errors=$((errors + 1)); }
    if [ -z "$BINARY" ]; then
        case "$(uname -m)" in
            x86_64 | amd64) ;;
            *) warn "release binaries are x86_64 only (found $(uname -m)); build from source and pass --binary"; errors=$((errors + 1)) ;;
        esac
        if ! has curl && ! has wget; then warn "curl or wget is needed to download"; errors=$((errors + 1)); fi
        if ! has sha256sum && ! has shasum; then warn "sha256sum or shasum is needed to verify the download"; errors=$((errors + 1)); fi
    elif [ ! -f "$BINARY" ]; then
        warn "--binary $BINARY: no such file"; errors=$((errors + 1))
    fi
    [ -n "${HOME:-}" ] || { warn "HOME is not set"; errors=$((errors + 1)); }

    if unsafe_path "$BIN_DIR"; then warn "install directory '$BIN_DIR' contains spaces or quotes, which desktop entries can't reference safely; use --bin-dir"; errors=$((errors + 1)); fi
    if unsafe_path "$DATA_HOME"; then warn "data directory '$DATA_HOME' contains spaces or quotes"; errors=$((errors + 1)); fi

    if [ "$NO_CONFIG" = 0 ]; then
        detect_package_manager
        if offer_package mpv mpv 1 "it plays the videos"; then
            say "  mpv:     $(mpv --version 2>/dev/null | head -n1 | cut -d' ' -f1-2)"
        else
            if [ -n "$PKG" ]; then hint="$(pkg_command) mpv"; else hint="your package manager"; fi
            warn "mpv not found: install it ($hint; on Fedora it comes from RPM Fusion) or pass --no-config"
            errors=$((errors + 1))
        fi
        if offer_package yt-dlp yt-dlp 0 "optional fallback when the built-in resolvers fail"; then
            say "  yt-dlp:  $(yt-dlp --version 2>/dev/null)"
        else
            say "  yt-dlp:  not found (optional: fallback when the built-in YouTube/Instagram resolvers fail)"
        fi
    fi

    if [ -z "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]; then
        warn "no graphical session in this shell; install from a terminal inside your desktop session for a meaningful check"
    fi
    if [ -n "${BROWSER:-}" ]; then
        warn "\$BROWSER is set ($BROWSER): programs that honour it open links without going through the default browser entry, so link-router won't see those links"
    fi

    [ "$errors" = 0 ] || die "$errors requirement(s) not met"
    say "  ok"
}

check_default_browser() {
    step "Checking the default browser"
    id=""
    if has xdg-mime; then
        id=$(xdg-mime query default x-scheme-handler/https 2>/dev/null || true)
    fi
    if [ -z "$id" ]; then
        if has xdg-mime; then
            warn "no default handler for https links; set your browser first, e.g. 'xdg-settings set default-web-browser firefox.desktop'"
            ask "Continue anyway? link-router will intercept once a default exists." n || die "aborted"
        else
            say "  xdg-mime not found; skipping ('link-router doctor' reports it after installation)"
        fi
        return 0
    fi
    file=$(find_desktop_file "$id" || true)
    say "  https links open with: $id${file:+ ($file)}"
    if [ -n "$file" ] && grep -q '^X-Link-Router-Shadow=' "$file"; then
        say "  already intercepted by link-router"
        return 0
    fi
    if [ -n "$file" ] && ! grep -q '^Categories=.*WebBrowser' "$file"; then
        warn "$id is not a web browser. link-router falls back to the default handler for every link it doesn't play, so make your browser the default first, e.g. 'xdg-settings set default-web-browser firefox.desktop'"
        ask "Continue with $id as the fallback?" n || die "aborted: set your browser as default and rerun"
    fi
}

install_binary() {
    step "Installing the binary"
    mkdir -p "$BIN_DIR"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM
    if [ -n "$BINARY" ]; then
        cp "$BINARY" "$tmp/$ASSET"
        say "  from $BINARY"
    else
        if [ -n "${LINK_ROUTER_BASE_URL:-}" ]; then
            base=${LINK_ROUTER_BASE_URL%/}
        elif [ "$VERSION" = latest ]; then
            base="https://github.com/$REPO/releases/latest/download"
        else
            base="https://github.com/$REPO/releases/download/v${VERSION#v}"
        fi
        say "  downloading $base/$ASSET"
        fetch "$base/$ASSET" "$tmp/$ASSET" || die "download failed: $base/$ASSET"
        fetch "$base/$ASSET.sha256" "$tmp/$ASSET.sha256" || die "download failed: $base/$ASSET.sha256"
        expected=$(cut -d' ' -f1 <"$tmp/$ASSET.sha256")
        actual=$(sha256_of "$tmp/$ASSET")
        [ -n "$expected" ] && [ "$expected" = "$actual" ] || die "checksum mismatch for $ASSET (expected $expected, got $actual)"
        say "  checksum ok"
    fi
    chmod 755 "$tmp/$ASSET"
    "$tmp/$ASSET" version >/dev/null 2>&1 || die "the binary doesn't run on this system"

    if [ -x "$BIN_DIR/link-router" ]; then
        say "  replacing $("$BIN_DIR/link-router" version 2>/dev/null || echo 'an existing install')"
        "$BIN_DIR/link-router" stop >/dev/null 2>&1 || true
    fi
    # Same-directory rename: a running client never sees a half-written file.
    cp "$tmp/$ASSET" "$BIN_DIR/.link-router.new"
    mv -f "$BIN_DIR/.link-router.new" "$BIN_DIR/link-router"
    say "  $("$BIN_DIR/link-router" version) -> $BIN_DIR/link-router"
}

config_path() { printf '%s\n' "$CONFIG_HOME/link-router/init.lua"; }

lua_bool() { if [ "$1" = 1 ]; then printf true; else printf false; fi; }

# Decides SRC_YOUTUBE, SRC_INSTAGRAM and SRC_DIRECT (1 or 0) for a new config.
choose_sources() {
    [ "$NO_CONFIG" = 0 ] || return 0
    if [ -f "$(config_path)" ]; then
        if [ "$SOURCES_GIVEN" = 1 ]; then
            warn "keeping the existing $(config_path); --youtube/--instagram/--direct/--all only shape a new config (edit 'sites' there instead)"
        fi
        return 0
    fi
    if [ "$SOURCES_GIVEN" = 1 ]; then
        : "${SRC_YOUTUBE:=0}" "${SRC_INSTAGRAM:=0}" "${SRC_DIRECT:=0}"
    elif [ "$ASSUME_YES" = 1 ] || ! tty_ok; then
        SRC_YOUTUBE=1 SRC_INSTAGRAM=1 SRC_DIRECT=1
    else
        step "Which links should play in mpv?"
        if ask "  YouTube (videos, shorts, live, embeds)?" y; then SRC_YOUTUBE=1; else SRC_YOUTUBE=0; fi
        if ask "  Instagram (reels, video posts)?" y; then SRC_INSTAGRAM=1; else SRC_INSTAGRAM=0; fi
        if ask "  Direct media links (.mp4, .webm, .mkv, .mov, .m3u8, ...)?" y; then SRC_DIRECT=1; else SRC_DIRECT=0; fi
    fi
    if [ "$SRC_YOUTUBE$SRC_INSTAGRAM$SRC_DIRECT" = 000 ]; then
        say "  no sources chosen: no config is written and every link passes through to the browser"
        NO_CONFIG=1
    fi
}

write_config() {
    [ "$NO_CONFIG" = 0 ] || return 0
    config=$(config_path)
    step "Config"
    if [ -f "$config" ]; then
        say "  keeping $config"
        return 0
    fi
    mkdir -p "$CONFIG_HOME/link-router"
    cat >"$config" <<EOF
-- link-router config. Changes apply when the daemon restarts: run
-- \`link-router stop\`; the service manager or the next link starts it again.

router.use("mpv-video", {
  player = {
    -- Extra mpv arguments, e.g. { "--vo=gpu-next", "--hwdec=vaapi" }.
    args = {},
    -- Extra environment for mpv; \`false\` unsets a variable.
    -- Hybrid-GPU laptops rendering on Intel: { LIBVA_DRIVER_NAME = "iHD" }.
    env = {},
  },
  quality = {
    max_resolution = 1080, -- shorter side: a 720x1280 short counts as 720
    max_fps = 60,
    buffer = "1s",         -- raise on slow networks, 0 to start immediately
  },
  window = {
    box = { 960, 720 },    -- the video is fitted into this box
    margin = { 35, 25 },   -- from the bottom-right corner
  },
  sites = {
    youtube = $(lua_bool "$SRC_YOUTUBE"),
    instagram = $(lua_bool "$SRC_INSTAGRAM"),
    -- true for the built-in extension list, false, or a list: { "mp4", "webm" }
    direct = $(lua_bool "$SRC_DIRECT"),
  },
})
EOF
    say "  wrote $config (youtube=$(lua_bool "$SRC_YOUTUBE") instagram=$(lua_bool "$SRC_INSTAGRAM") direct=$(lua_bool "$SRC_DIRECT"))"
}

enable_interception() {
    if [ "$NO_ENABLE" = 1 ]; then
        step "Not enabling (--no-enable); run 'link-router enable' when ready"
        return 0
    fi
    step "Enabling interception"
    if [ "$ASSUME_YES" = 1 ]; then
        "$BIN_DIR/link-router" enable --yes
    elif tty_ok; then
        "$BIN_DIR/link-router" enable </dev/tty
    else
        "$BIN_DIR/link-router" enable </dev/null
    fi
}

# Single-quoted for sh, e.g. it's -> 'it'\''s'.
sh_quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

# XDG directories set in this session: service managers start the daemon
# outside it, so non-default locations have to be passed on.
xdg_overrides() {
    for var in XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME XDG_CONFIG_DIRS XDG_DATA_DIRS; do
        eval "val=\${$var:-}"
        # shellcheck disable=SC2154
        [ -z "$val" ] || printf '%s=%s\n' "$var" "$val"
    done
}

detect_service_manager() {
    if has systemctl && systemctl --user show-environment >/dev/null 2>&1; then
        printf systemd
    elif has rc-service && [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -f "$XDG_RUNTIME_DIR/openrc/softlevel" ]; then
        printf openrc
    else
        printf autostart
    fi
}

service_systemd() {
    unit="$CONFIG_HOME/systemd/user/link-router.service"
    mkdir -p "$(dirname "$unit")"
    {
        printf '%s\n' "[Unit]" "Description=link-router daemon" "Documentation=https://github.com/$REPO" ""
        printf '%s\n' "[Service]" "ExecStart=$BIN_DIR/link-router daemon --resident"
        xdg_overrides | while IFS= read -r kv; do printf 'Environment="%s"\n' "$kv"; done
        printf '%s\n' "Restart=always" "RestartSec=1" "" "[Install]" "WantedBy=default.target"
    } >"$unit"
    say "  wrote $unit"
    systemctl --user daemon-reload
    systemctl --user enable link-router.service
    systemctl --user restart link-router.service
}

service_openrc() {
    script="$CONFIG_HOME/rc/init.d/link-router"
    mkdir -p "$CONFIG_HOME/rc/init.d" "$CONFIG_HOME/rc/runlevels/default"
    runner=$(command -v openrc-run || printf /sbin/openrc-run)
    {
        printf '%s\n' "#!$runner" "description=\"link-router daemon\"" "supervisor=supervise-daemon"
        printf 'command=%s\n' "$(sh_quote "$BIN_DIR/link-router")"
        printf '%s\n' 'command_args="daemon --resident"'
        xdg_overrides | while IFS= read -r kv; do printf 'export %s=%s\n' "${kv%%=*}" "$(sh_quote "${kv#*=}")"; done
    } >"$script"
    chmod 755 "$script"
    say "  wrote $script"
    rc-update --user add link-router default >/dev/null
    if rc-service --user link-router status >/dev/null 2>&1; then
        rc-service --user link-router restart
    else
        rc-service --user link-router start
    fi
}

service_autostart() {
    entry="$CONFIG_HOME/autostart/link-router.desktop"
    mkdir -p "$CONFIG_HOME/autostart"
    printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=link-router" \
        "Comment=Link interception sentinel and video player daemon" \
        "Exec=$BIN_DIR/link-router daemon --resident" "NoDisplay=true" "Terminal=false" >"$entry"
    say "  wrote $entry"
    if has setsid; then
        setsid "$BIN_DIR/link-router" daemon --resident </dev/null >/dev/null 2>&1 &
    else
        nohup "$BIN_DIR/link-router" daemon --resident </dev/null >/dev/null 2>&1 &
    fi
    say "  started the daemon for this session"
    if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}${SWAYSOCK:-}${NIRI_SOCKET:-}" ] || [ "${XDG_CURRENT_DESKTOP:-}" = river ]; then
        say "  note: this compositor doesn't run XDG autostart entries by itself; add"
        say "        '$BIN_DIR/link-router daemon --resident' to its startup commands (exec-once)"
    fi
}

register_service() {
    [ "$NO_ENABLE" = 0 ] || return 0
    kind=$SERVICE
    [ "$kind" != auto ] || kind=$(detect_service_manager)
    step "Daemon service ($kind)"
    case "$kind" in
        systemd) service_systemd ;;
        openrc) service_openrc ;;
        autostart) service_autostart ;;
        none) say "  none: the first link starts the daemon, which exits after 30 idle minutes" ;;
        *) die "unknown --service $kind (auto, systemd, openrc, autostart, none)" ;;
    esac
}

finish() {
    step "Status"
    sleep 1
    "$BIN_DIR/link-router" doctor || true
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say ""; say "note: $BIN_DIR is not in PATH; desktop entries don't need it, but add it to run 'link-router' by name" ;;
    esac
    say ""
    say "Done. Click a YouTube link to try it. Logs: $STATE_HOME/link-router/daemon.log"
    say "Uninstall: curl -fsSL https://raw.githubusercontent.com/$REPO/main/uninstall.sh | sh"
}

main() {
    ASSUME_YES=0 NO_ENABLE=0 NO_CONFIG=0 NO_DEPS=0 BINARY="" SERVICE=auto
    SOURCES_GIVEN=0 SRC_YOUTUBE="" SRC_INSTAGRAM="" SRC_DIRECT=""
    VERSION=${LINK_ROUTER_VERSION:-$DEFAULT_VERSION}
    BIN_DIR=${LINK_ROUTER_BIN_DIR:-${HOME:-}/.local/bin}
    while [ $# -gt 0 ]; do
        case "$1" in
            --yes | -y) ASSUME_YES=1 ;;
            --no-enable) NO_ENABLE=1 ;;
            --no-config) NO_CONFIG=1 ;;
            --youtube) SOURCES_GIVEN=1 SRC_YOUTUBE=1 ;;
            --instagram) SOURCES_GIVEN=1 SRC_INSTAGRAM=1 ;;
            --direct) SOURCES_GIVEN=1 SRC_DIRECT=1 ;;
            --all) SOURCES_GIVEN=1 SRC_YOUTUBE=1 SRC_INSTAGRAM=1 SRC_DIRECT=1 ;;
            --no-deps) NO_DEPS=1 ;;
            --service) [ $# -ge 2 ] || die "--service needs a value"; SERVICE=$2; shift ;;
            --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=$2; shift ;;
            --binary) [ $# -ge 2 ] || die "--binary needs a path"; BINARY=$2; shift ;;
            --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a path"; BIN_DIR=$2; shift ;;
            -h | --help) usage; exit 0 ;;
            *) usage >&2; die "unknown option: $1" ;;
        esac
        shift
    done
    CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
    DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
    STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}

    choose_sources
    check_requirements
    check_default_browser
    install_binary
    write_config
    enable_interception
    register_service
    finish
}

# The whole script is parsed before anything runs, so a truncated download does nothing.
main "$@"
