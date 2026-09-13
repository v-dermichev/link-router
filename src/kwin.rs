//! KWin adapter (KDE Plasma 6): a KWin script, loaded once over D-Bus, keeps the
//! player window in the bottom-right corner. mpv sizes the window itself from
//! its `geometry` option, which the daemon sets before each file; the script
//! positions it when it opens, whenever its size changes, and moves it to the
//! current desktop and screen when a new file starts (mpv changes the title).

use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::VideoConfig;
use crate::paths;

const PLUGIN_NAME: &str = "link-router";

const SCRIPT: &str = r#"// link-router player placement, loaded by the link-router daemon over D-Bus.
const APP_ID = @APP_ID@.toLowerCase();
const MARGIN_X = @MARGIN_X@;
const MARGIN_Y = @MARGIN_Y@;
const FOLLOW_FOCUS = @FOLLOW_FOCUS@;
const hooked = {};

function isPlayer(w) {
    return !!w && (String(w.resourceClass).toLowerCase() === APP_ID || String(w.resourceName).toLowerCase() === APP_ID);
}

function setGeometry(w, x, y) {
    w.frameGeometry = { x: x, y: y, width: w.width, height: w.height };
    if (Math.round(w.x) !== x || Math.round(w.y) !== y) {
        // Geometry types that don't convert from plain objects: edit a copy of the window's own value.
        const g = w.frameGeometry;
        g.x = x;
        g.y = y;
        w.frameGeometry = g;
    }
}

// Fullscreen and maximized windows belong to the user; KWin may report the
// fullscreen size before the fullScreen flag, hence the screen-size check.
function userSized(w) {
    if (w.fullScreen || (w.maximizeMode !== undefined && w.maximizeMode !== 0)) return true;
    const output = w.output || workspace.activeScreen;
    const screen = workspace.clientArea(KWin.FullScreenArea, output, workspace.currentDesktop);
    return w.width >= screen.width && w.height >= screen.height;
}

function anchor(w, toActive) {
    const output = toActive || !w.output ? workspace.activeScreen : w.output;
    const desktop = toActive || !w.desktops || w.desktops.length === 0 ? workspace.currentDesktop : w.desktops[0];
    const area = workspace.clientArea(KWin.PlacementArea, output, desktop);
    setGeometry(w, Math.round(area.x + area.width - MARGIN_X - w.width), Math.round(area.y + area.height - MARGIN_Y - w.height));
}

function hook(w) {
    if (!isPlayer(w)) return;
    const id = String(w.internalId);
    if (hooked[id]) return;
    hooked[id] = true;
    let width = w.width;
    let height = w.height;
    w.keepAbove = true;
    if (FOLLOW_FOCUS) w.desktops = [workspace.currentDesktop];
    anchor(w, FOLLOW_FOCUS);
    w.frameGeometryChanged.connect(function () {
        if (w.width === width && w.height === height) return;
        width = w.width;
        height = w.height;
        // A size the user drags stays where the user puts it.
        if (w.move || w.resize || userSized(w)) return;
        anchor(w, false);
    });
    w.captionChanged.connect(function () {
        if (!FOLLOW_FOCUS || userSized(w)) return;
        w.desktops = [workspace.currentDesktop];
        anchor(w, true);
    });
    w.closed.connect(function () {
        delete hooked[id];
    });
}

workspace.windowAdded.connect(hook);
workspace.windowList().forEach(hook);
console.info("link-router: placement script active for " + APP_ID);
"#;

static LOADED: AtomicBool = AtomicBool::new(false);

/// Whether the link came from a KDE Plasma session.
pub fn is_kde(env: &BTreeMap<String, String>) -> bool {
    env.get("XDG_CURRENT_DESKTOP")
        .map(|d| d.split(':').any(|n| n.eq_ignore_ascii_case("KDE")))
        .unwrap_or(false)
}

pub fn script(cfg: &VideoConfig) -> String {
    SCRIPT
        .replace("@APP_ID@", &serde_json::to_string(&cfg.app_id).unwrap_or_else(|_| "\"\"".into()))
        .replace("@MARGIN_X@", &cfg.margin.0.to_string())
        .replace("@MARGIN_Y@", &cfg.margin.1.to_string())
        .replace("@FOLLOW_FOCUS@", if cfg.follow_focus { "true" } else { "false" })
}

async fn dbus(env: &BTreeMap<String, String>, path: &str, method: &str, args: &[String]) -> Result<String> {
    let mut cmd = tokio::process::Command::new("dbus-send");
    cmd.args(["--session", "--print-reply", "--dest=org.kde.KWin", path, method]).args(args);
    if let Some(bus) = env.get("DBUS_SESSION_BUS_ADDRESS") {
        cmd.env("DBUS_SESSION_BUS_ADDRESS", bus);
    }
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(2), cmd.output())
        .await
        .map_err(|_| anyhow!("dbus-send {method} timed out"))??;
    if !out.status.success() {
        bail!("dbus-send {method}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Loads the placement script into KWin unless this daemon already did and it
/// is still loaded (KWin may have restarted). The first load in a daemon
/// replaces any copy a previous daemon left, which may carry other options.
pub async fn ensure_script(cfg: &VideoConfig, env: &BTreeMap<String, String>) -> Result<()> {
    let name = format!("string:{PLUGIN_NAME}");
    if LOADED.load(Ordering::SeqCst) {
        let reply = dbus(env, "/Scripting", "org.kde.kwin.Scripting.isScriptLoaded", &[name.clone()]).await?;
        if reply.contains("boolean true") {
            return Ok(());
        }
    }
    let path = paths::runtime_dir().join("kwin-link-router.js");
    std::fs::create_dir_all(paths::runtime_dir())?;
    std::fs::write(&path, script(cfg))?;
    dbus(env, "/Scripting", "org.kde.kwin.Scripting.unloadScript", &[name.clone()]).await?;
    let reply = dbus(env, "/Scripting", "org.kde.kwin.Scripting.loadScript", &[format!("string:{}", path.display()), name]).await?;
    let id: i64 = reply
        .split_whitespace()
        .skip_while(|w| *w != "int32")
        .nth(1)
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("unexpected loadScript reply: {}", reply.trim()))?;
    if id < 0 {
        bail!("KWin refused the placement script");
    }
    dbus(env, &format!("/Scripting/Script{id}"), "org.kde.kwin.Script.run", &[]).await?;
    LOADED.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_script_options() {
        let cfg = VideoConfig { app_id: "a\"b".into(), margin: (7, 9), follow_focus: false, ..Default::default() };
        let s = script(&cfg);
        assert!(s.contains(r#"const APP_ID = "a\"b".toLowerCase();"#));
        assert!(s.contains("const MARGIN_X = 7;") && s.contains("const MARGIN_Y = 9;"));
        assert!(s.contains("const FOLLOW_FOCUS = false;"));
        assert!(!s.contains('@'));
    }

    #[test]
    fn detects_kde_sessions() {
        let env = |v: &str| BTreeMap::from([("XDG_CURRENT_DESKTOP".to_string(), v.to_string())]);
        assert!(is_kde(&env("KDE")));
        assert!(is_kde(&env("ubuntu:KDE")));
        assert!(!is_kde(&env("Hyprland")));
        assert!(!is_kde(&BTreeMap::new()));
    }
}
