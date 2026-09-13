#!/bin/sh
# link-router uninstaller.
#
#   curl -fsSL https://raw.githubusercontent.com/v-dermichev/link-router/main/uninstall.sh | sh
#
# Restores the browser desktop entries link-router shadowed, removes the daemon
# service, the binary, data, logs and cache. The config is kept unless --purge.

set -eu

usage() {
    cat <<USAGE
Usage: uninstall.sh [options]

  --purge          also delete the config directory (~/.config/link-router)
  --bin-dir DIR    where the binary is, when it isn't found automatically
  -h, --help       show this help
USAGE
}

say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

find_binary() {
    if [ -n "$BIN_DIR" ]; then
        printf '%s\n' "$BIN_DIR/link-router"
        return 0
    fi
    state="$STATE_HOME/link-router/state.json"
    if [ -f "$state" ]; then
        path=$(sed -n 's/.*"install_path": *"\([^"]*\)".*/\1/p' "$state")
        if [ -n "$path" ] && [ -x "$path" ]; then
            printf '%s\n' "$path"
            return 0
        fi
    fi
    if has link-router; then
        command -v link-router
        return 0
    fi
    printf '%s\n' "$HOME/.local/bin/link-router"
}

remove_services() {
    step "Removing the daemon service"
    found=0
    unit="$CONFIG_HOME/systemd/user/link-router.service"
    if [ -f "$unit" ]; then
        found=1
        if has systemctl; then
            systemctl --user disable --now link-router.service 2>/dev/null || true
        fi
        rm -f "$unit"
        if has systemctl; then systemctl --user daemon-reload 2>/dev/null || true; fi
        say "  removed $unit"
    fi
    script="$CONFIG_HOME/rc/init.d/link-router"
    if [ -f "$script" ]; then
        found=1
        if has rc-service; then
            rc-service --user link-router stop 2>/dev/null || true
            rc-update --user del link-router default >/dev/null 2>&1 || true
        fi
        rm -f "$script" "$CONFIG_HOME/rc/runlevels/default/link-router"
        say "  removed $script"
    fi
    entry="$CONFIG_HOME/autostart/link-router.desktop"
    if [ -f "$entry" ]; then
        found=1
        rm -f "$entry"
        say "  removed $entry"
    fi
    [ "$found" = 1 ] || say "  none registered"
}

# Without a working binary: put user originals back and delete unmodified shadows.
restore_entries_by_hand() {
    apps="$DATA_HOME/applications"
    originals="$DATA_HOME/link-router/originals"
    for shadow in "$apps"/*.desktop; do
        [ -f "$shadow" ] || continue
        grep -q '^X-Link-Router-Shadow=' "$shadow" || continue
        id=$(basename "$shadow")
        if [ -f "$originals/$id" ]; then
            mv -f "$originals/$id" "$shadow"
            say "  $id: user original restored"
        else
            rm -f "$shadow"
            say "  $id: shadow removed"
        fi
    done
}

main() {
    PURGE=0 BIN_DIR=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --purge) PURGE=1 ;;
            --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a path"; BIN_DIR=$2; shift ;;
            -h | --help) usage; exit 0 ;;
            *) usage >&2; die "unknown option: $1" ;;
        esac
        shift
    done
    [ -n "${HOME:-}" ] || die "HOME is not set"
    CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
    DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
    STATE_HOME=${XDG_STATE_HOME:-$HOME/.local/state}
    CACHE_HOME=${XDG_CACHE_HOME:-$HOME/.cache}
    bin=$(find_binary)

    # Services first, so a supervisor doesn't restart the daemon that 'disable' stops.
    remove_services

    step "Restoring browser desktop entries"
    if [ -x "$bin" ] && "$bin" disable; then
        :
    else
        warn "couldn't run '$bin disable'; restoring entries directly"
        restore_entries_by_hand
    fi

    step "Removing files"
    if [ -n "$(ls -A "$DATA_HOME/link-router/originals" 2>/dev/null)" ]; then
        warn "$DATA_HOME/link-router/originals still holds desktop entries (a shadow was edited by hand?); keeping $DATA_HOME/link-router"
    else
        rm -rf "$DATA_HOME/link-router"
    fi
    rm -rf "$STATE_HOME/link-router" "$CACHE_HOME/link-router"
    [ -z "${XDG_RUNTIME_DIR:-}" ] || rm -rf "$XDG_RUNTIME_DIR/link-router"
    rm -rf "/tmp/link-router-$(id -u)"
    if [ -f "$bin" ]; then
        rm -f "$bin"
        say "  removed $bin"
    fi
    if [ "$PURGE" = 1 ]; then
        rm -rf "$CONFIG_HOME/link-router"
        say "  removed $CONFIG_HOME/link-router"
    elif [ -d "$CONFIG_HOME/link-router" ]; then
        say "  kept your config in $CONFIG_HOME/link-router (--purge deletes it)"
    fi
    say ""
    say "link-router is uninstalled; links open in your default browser directly."
}

main "$@"
