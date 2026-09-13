//! Interception: same-ID shadow desktop entries for the default handlers of the
//! intercepted schemes, their state, and the sentinel that keeps them intact.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::desktop::{self, DesktopEntry, MAIN_GROUP};
use crate::paths;

pub const SCHEMES: &[&str] = &["http", "https"];
const MARKER: &str = "X-Link-Router-Shadow";
const SOURCE_KEY: &str = "X-Link-Router-Source";
const SOURCE_HASH_KEY: &str = "X-Link-Router-Source-Hash";
const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// The original lives in `$XDG_DATA_DIRS` and is read from there.
    System,
    /// The original was the user's own file in `$XDG_DATA_HOME/applications`,
    /// moved to `originals/<id>`.
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shadow {
    pub source: PathBuf,
    pub origin: Origin,
    pub source_hash: String,
    pub shadow_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub enabled: bool,
    pub install_path: Option<PathBuf>,
    #[serde(default)]
    pub shadows: BTreeMap<String, Shadow>,
    #[serde(default)]
    pub last_sentinel: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: STATE_VERSION, enabled: false, install_path: None, shadows: BTreeMap::new(), last_sentinel: Vec::new() }
    }
}

impl State {
    pub fn load() -> State {
        fs::read(paths::state_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::state_file();
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn file_hash(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|b| sha256(&b))
}

pub fn by_id_path(id: &str) -> PathBuf {
    paths::by_id_dir().join(id)
}

pub fn is_marked(path: &Path) -> bool {
    DesktopEntry::load(path).map(|e| e.main(MARKER).is_some()).unwrap_or(false)
}

/// The entry a link through `id` falls back to: the recorded original, or the
/// effective entry when it isn't a shadow.
pub fn original_entry(id: &str, state: &State) -> Option<DesktopEntry> {
    if let Some(shadow) = state.shadows.get(id) {
        let path = match shadow.origin {
            Origin::User => paths::originals_dir().join(id),
            Origin::System => shadow.source.clone(),
        };
        if let Ok(e) = DesktopEntry::load(&path) {
            return Some(e);
        }
    }
    let effective = desktop::find_entry(id)?;
    let entry = DesktopEntry::load(&effective).ok()?;
    if entry.main(MARKER).is_none() {
        return Some(entry);
    }
    let source = entry.main(SOURCE_KEY).map(PathBuf::from)?;
    DesktopEntry::load(&source).ok()
}

/// Shadow text: the original with only the main `Exec` replaced, D-Bus
/// activation off, `TryExec` pointing at the by-id link when the original has
/// none, and marker keys.
pub fn shadow_text(original: &str, id: &str, source: &Path, source_hash: &str) -> String {
    let by_id = by_id_path(id);
    let mut out = String::with_capacity(original.len() + 256);
    let mut group = String::new();
    let lines: Vec<&str> = original.lines().collect();
    let has_tryexec = {
        let mut g = String::new();
        lines.iter().any(|l| {
            let t = l.trim();
            if let Some(name) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                g = name.to_string();
            }
            g == MAIN_GROUP && t.starts_with("TryExec=")
        })
    };
    for line in lines {
        let trimmed = line.trim_start();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            group = name.to_string();
            out.push_str(line);
            out.push('\n');
            if group == MAIN_GROUP {
                out.push_str(&format!("{MARKER}=1\n{SOURCE_KEY}={}\n{SOURCE_HASH_KEY}={source_hash}\n", source.display()));
                out.push_str("DBusActivatable=false\n");
                if !has_tryexec {
                    out.push_str(&format!("TryExec={}\n", by_id.display()));
                }
            }
            continue;
        }
        if group == MAIN_GROUP {
            let key = trimmed.split('=').next().unwrap_or("").trim();
            match key {
                "Exec" => {
                    out.push_str(&format!("Exec={} %U\n", by_id.display()));
                    continue;
                }
                "DBusActivatable" | MARKER | SOURCE_KEY | SOURCE_HASH_KEY => continue,
                _ => {}
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn ensure_by_id(id: &str, install_path: &Path) -> Result<()> {
    fs::create_dir_all(paths::by_id_dir())?;
    let link = by_id_path(id);
    if fs::read_link(&link).ok().as_deref() == Some(install_path) {
        return Ok(());
    }
    let _ = fs::remove_file(&link);
    symlink(install_path, &link).with_context(|| format!("linking {}", link.display()))
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("desktop.link-router-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Default handler IDs of every intercepted scheme.
pub fn default_ids() -> BTreeSet<String> {
    SCHEMES.iter().filter_map(|s| desktop::default_handler(s)).collect()
}

/// What to do with the user's own unmarked override of a default entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UserOverride {
    Replace,
    Leave,
    Ask,
}

/// One sentinel pass. Returns log lines; saves state when anything changed.
pub fn sentinel(state: &mut State, user_override: UserOverride) -> Result<Vec<String>> {
    let mut log = Vec::new();
    if !state.enabled {
        log.push("interception not enabled; nothing to do".into());
        return Ok(log);
    }
    let install = state.install_path.clone().ok_or_else(|| anyhow!("state has no install_path"))?;
    if !install.is_file() {
        bail!("recorded install path {} is missing; run `link-router enable` from the new location", install.display());
    }
    let user_apps = paths::user_applications();
    let wanted = default_ids();
    let mut changed = false;

    for id in &wanted {
        let Some(effective) = desktop::find_entry(id) else {
            log.push(format!("{id}: default has no desktop file"));
            continue;
        };
        ensure_by_id(id, &install)?;
        let entry = DesktopEntry::load(&effective)?;
        if entry.main(MARKER).is_some() {
            let record = state.shadows.get(id).cloned();
            let source = entry.main(SOURCE_KEY).map(PathBuf::from);
            let Some(source) = source else { continue };
            if let Some(rec) = &record {
                if file_hash(&effective).as_deref() != Some(rec.shadow_hash.as_str()) {
                    log.push(format!("{id}: shadow edited outside link-router; left as is"));
                    continue;
                }
            }
            let current = file_hash(&source);
            let recorded = entry.main(SOURCE_HASH_KEY).map(String::from);
            if current.is_some() && current != recorded {
                let origin = record.map(|r| r.origin).unwrap_or(Origin::System);
                let text = fs::read_to_string(&source)?;
                let hash = current.unwrap();
                let shadow = shadow_text(&text, id, &source, &hash);
                write_atomic(&effective, &shadow)?;
                state.shadows.insert(id.clone(), Shadow { source, origin, source_hash: hash, shadow_hash: sha256(shadow.as_bytes()) });
                log.push(format!("{id}: original changed; shadow refreshed"));
                changed = true;
            }
            continue;
        }
        let target = user_apps.join(id);
        if effective.starts_with(&user_apps) {
            let replace = match user_override {
                UserOverride::Replace => true,
                UserOverride::Leave => false,
                UserOverride::Ask => ask(&format!(
                    "{} is your own override of the default handler {id}. Move it to {} and shadow it? [y/N] ",
                    effective.display(),
                    paths::originals_dir().display()
                )),
            };
            if !replace {
                log.push(format!("{id}: user override at {} not replaced (needs consent)", effective.display()));
                continue;
            }
            fs::create_dir_all(paths::originals_dir())?;
            let kept = paths::originals_dir().join(id);
            fs::copy(&effective, &kept)?;
            let text = fs::read_to_string(&kept)?;
            let hash = sha256(text.as_bytes());
            let shadow = shadow_text(&text, id, &kept, &hash);
            write_atomic(&target, &shadow)?;
            state.shadows.insert(id.clone(), Shadow { source: kept, origin: Origin::User, source_hash: hash, shadow_hash: sha256(shadow.as_bytes()) });
            log.push(format!("{id}: user override moved to originals and shadowed"));
        } else {
            let text = fs::read_to_string(&effective)?;
            let hash = sha256(text.as_bytes());
            let shadow = shadow_text(&text, id, &effective, &hash);
            write_atomic(&target, &shadow)?;
            state.shadows.insert(id.clone(), Shadow { source: effective.clone(), origin: Origin::System, source_hash: hash, shadow_hash: sha256(shadow.as_bytes()) });
            log.push(format!("{id}: shadowed {}", effective.display()));
        }
        changed = true;
    }

    let stale: Vec<String> = state.shadows.keys().filter(|id| !wanted.contains(*id)).cloned().collect();
    for id in stale {
        if remove_shadow(state, &id, &mut log)? {
            changed = true;
        }
    }

    state.last_sentinel = log.clone();
    if changed {
        state.save()?;
    }
    Ok(log)
}

/// Removes one shadow if it is still unmodified, restoring a moved user original.
fn remove_shadow(state: &mut State, id: &str, log: &mut Vec<String>) -> Result<bool> {
    let Some(rec) = state.shadows.get(id).cloned() else { return Ok(false) };
    let path = paths::user_applications().join(id);
    let unmodified = file_hash(&path).as_deref() == Some(rec.shadow_hash.as_str());
    if path.exists() && !unmodified {
        log.push(format!("{id}: shadow was modified; not removed"));
        state.shadows.remove(id);
        return Ok(true);
    }
    match rec.origin {
        Origin::User => {
            let kept = paths::originals_dir().join(id);
            if kept.is_file() {
                fs::rename(&kept, &path)?;
                log.push(format!("{id}: user original restored"));
            }
        }
        Origin::System => {
            let _ = fs::remove_file(&path);
            log.push(format!("{id}: shadow removed"));
        }
    }
    let _ = fs::remove_file(by_id_path(id));
    state.shadows.remove(id);
    Ok(true)
}

fn ask(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

pub fn enable(assume_yes: bool) -> Result<()> {
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let exe_str = exe.to_string_lossy();
    if exe_str.chars().any(|c| c.is_whitespace() || "\"'\\$`%".contains(c)) {
        bail!("install path {exe_str} contains whitespace or quoting characters");
    }
    let by_id = paths::by_id_dir();
    if by_id.to_string_lossy().chars().any(|c| c.is_whitespace() || "\"'\\$`%".contains(c)) {
        bail!("data directory {} contains whitespace or quoting characters", by_id.display());
    }
    let mut state = State::load();
    state.enabled = true;
    state.install_path = Some(exe);
    state.save()?;
    let policy = if assume_yes { UserOverride::Replace } else { UserOverride::Ask };
    for line in sentinel(&mut state, policy)? {
        println!("{line}");
    }
    state.save()?;
    Ok(())
}

pub fn disable() -> Result<()> {
    crate::client::shutdown_daemon();
    let mut state = State::load();
    state.enabled = false;
    let ids: Vec<String> = state.shadows.keys().cloned().collect();
    let mut log = Vec::new();
    for id in ids {
        remove_shadow(&mut state, &id, &mut log)?;
    }
    state.save()?;
    for line in log {
        println!("{line}");
    }
    println!("interception disabled");
    Ok(())
}

pub fn doctor() -> Result<()> {
    let state = State::load();
    println!("enabled: {}", state.enabled);
    println!("install path: {}", state.install_path.as_ref().map(|p| p.display().to_string()).unwrap_or("-".into()));
    for scheme in SCHEMES {
        let mime = format!("x-scheme-handler/{scheme}");
        match desktop::default_handler(scheme) {
            None => println!("{mime}: no default handler"),
            Some(id) => {
                let effective = desktop::find_entry(&id);
                let marked = effective.as_deref().map(is_marked).unwrap_or(false);
                let link_ok = state.install_path.is_some() && fs::read_link(by_id_path(&id)).ok() == state.install_path;
                println!(
                    "{mime}: {id} -> {} [{}] by-id link {}",
                    effective.as_ref().map(|p| p.display().to_string()).unwrap_or("missing".into()),
                    if marked { "shadow" } else { "NOT shadowed" },
                    if link_ok { "ok" } else { "missing/wrong" }
                );
                if let Ok(out) = std::process::Command::new("xdg-settings").args(["check", "default-web-browser", &id]).output() {
                    println!("  xdg-settings check default-web-browser {id}: {}", String::from_utf8_lossy(&out.stdout).trim());
                }
            }
        }
    }
    if let Some(kde) = desktop::kde_browser_setting().filter(|v| !v.is_empty()) {
        println!("KDE BrowserApplication: {kde}");
        if kde.starts_with('!') {
            println!("warning: a command as KDE browser can't be intercepted; KDE apps use it only when no http(s) default handler is set");
        }
    }
    if let Ok(b) = std::env::var("BROWSER") {
        println!("warning: $BROWSER={b} (programs honouring it skip desktop entries)");
    }
    println!("daemon: {}", if crate::client::daemon_alive() { "running" } else { "not running" });
    for line in &state.last_sentinel {
        println!("last sentinel: {line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_replaces_main_exec_only() {
        let original = "[Desktop Entry]\nName=B\nExec=brave %U\nDBusActivatable=true\n[Desktop Action new]\nExec=brave --new-window\n";
        let s = shadow_text(original, "brave-browser.desktop", Path::new("/usr/share/applications/brave-browser.desktop"), "h");
        assert!(s.contains(&format!("Exec={} %U", by_id_path("brave-browser.desktop").display())));
        assert!(s.contains("Exec=brave --new-window"));
        assert!(s.contains("DBusActivatable=false"));
        assert!(!s.contains("DBusActivatable=true"));
        assert!(s.contains("X-Link-Router-Shadow=1"));
        assert!(s.contains("TryExec="));
    }
}
