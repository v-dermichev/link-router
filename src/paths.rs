use std::env;
use std::path::PathBuf;

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

fn xdg_list(var: &str, fallback: &str) -> Vec<PathBuf> {
    let value = env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| fallback.to_string());
    value.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn config_dirs() -> Vec<PathBuf> {
    xdg_list("XDG_CONFIG_DIRS", "/etc/xdg")
}

pub fn data_dirs() -> Vec<PathBuf> {
    xdg_list("XDG_DATA_DIRS", "/usr/local/share:/usr/share")
}

/// Runtime directory for sockets. Unix socket paths are limited to 108 bytes, so a
/// long `$XDG_RUNTIME_DIR` falls back to a per-user directory under `/tmp`.
pub fn runtime_dir() -> PathBuf {
    let preferred = env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("link-router"));
    match preferred {
        Some(p) if p.as_os_str().len() + "/daemon.sock".len() < 100 => p,
        _ => PathBuf::from(format!("/tmp/link-router-{}", unsafe { libc::getuid() })),
    }
}

pub fn socket() -> PathBuf {
    runtime_dir().join("daemon.sock")
}

pub fn user_applications() -> PathBuf {
    data_home().join("applications")
}

/// Application directories in lookup order: `$XDG_DATA_HOME` first.
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![user_applications()];
    dirs.extend(data_dirs().into_iter().map(|d| d.join("applications")));
    dirs
}

/// Directories holding mimeapps lists, in lookup order.
pub fn mimeapps_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![config_home()];
    dirs.extend(config_dirs());
    dirs.push(user_applications());
    dirs.extend(data_dirs().into_iter().map(|d| d.join("applications")));
    dirs
}

pub fn lr_data() -> PathBuf {
    data_home().join("link-router")
}

pub fn by_id_dir() -> PathBuf {
    lr_data().join("by-id")
}

pub fn originals_dir() -> PathBuf {
    lr_data().join("originals")
}

pub fn state_dir() -> PathBuf {
    state_home().join("link-router")
}

pub fn state_file() -> PathBuf {
    state_dir().join("state.json")
}

pub fn log_file() -> PathBuf {
    state_dir().join("daemon.log")
}

pub fn config_file() -> PathBuf {
    config_home().join("link-router").join("init.lua")
}

pub fn cache_dir() -> PathBuf {
    cache_home().join("link-router")
}
