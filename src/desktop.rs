//! Desktop entries: parsing, `Exec` expansion, lookup by ID, default handlers
//! and launching (Desktop Entry Specification 1.5, MIME Applications Associations 1.0.1).

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::paths;

pub const MAIN_GROUP: &str = "Desktop Entry";

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub path: PathBuf,
    groups: Vec<(String, Vec<(String, String)>)>,
}

impl DesktopEntry {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Self::parse(path.to_path_buf(), text))
    }

    pub fn parse(path: PathBuf, text: String) -> Self {
        let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for line in text.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                groups.push((name.to_string(), Vec::new()));
            } else if let Some((key, value)) = line.split_once('=') {
                if let Some((_, entries)) = groups.last_mut() {
                    entries.push((key.trim_end().to_string(), value.trim_start().to_string()));
                }
            }
        }
        Self { path, groups }
    }

    pub fn get(&self, group: &str, key: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|(name, _)| name == group)
            .and_then(|(_, entries)| entries.iter().find(|(k, _)| k == key))
            .map(|(_, v)| v.as_str())
    }

    pub fn main(&self, key: &str) -> Option<&str> {
        self.get(MAIN_GROUP, key)
    }

    pub fn exec_argv(&self) -> Result<Vec<String>> {
        let exec = self.main("Exec").ok_or_else(|| anyhow!("{} has no Exec", self.path.display()))?;
        tokenize_exec(&unescape_value(exec))
    }
}

/// `\s`, `\n`, `\t`, `\r`, `\\` escapes of string values.
pub fn unescape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits an `Exec` value into arguments: whitespace separates, double quotes
/// group, and inside quotes a backslash escapes `"`, `` ` ``, `$` and `\`.
pub fn tokenize_exec(exec: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => match chars.next() {
                    Some(e @ ('"' | '`' | '$' | '\\')) => current.push(e),
                    Some(other) => {
                        current.push('\\');
                        current.push(other);
                    }
                    None => bail!("unterminated escape in Exec"),
                },
                _ => current.push(c),
            }
        } else if c == '"' {
            quoted = true;
            in_arg = true;
        } else if c.is_whitespace() {
            if in_arg {
                args.push(std::mem::take(&mut current));
                in_arg = false;
            }
        } else {
            current.push(c);
            in_arg = true;
        }
    }
    if quoted {
        bail!("unterminated quote in Exec");
    }
    if in_arg {
        args.push(current);
    }
    if args.is_empty() {
        bail!("empty Exec");
    }
    Ok(args)
}

/// Expands field codes. `%f`/`%F` receive local paths for `file://` URLs and the
/// URL itself otherwise.
pub fn expand_exec(argv: &[String], urls: &[String], entry: &DesktopEntry) -> Vec<String> {
    let as_file = |u: &String| -> String {
        url::Url::parse(u)
            .ok()
            .filter(|p| p.scheme() == "file")
            .and_then(|p| p.to_file_path().ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| u.clone())
    };
    let mut out = Vec::new();
    for arg in argv {
        match arg.as_str() {
            "%U" => out.extend(urls.iter().cloned()),
            "%F" => out.extend(urls.iter().map(as_file)),
            "%u" => out.extend(urls.first().cloned()),
            "%f" => out.extend(urls.first().map(as_file)),
            "%i" => {
                if let Some(icon) = entry.main("Icon") {
                    out.push("--icon".into());
                    out.push(icon.into());
                }
            }
            "%c" => out.extend(entry.main("Name").map(String::from)),
            "%k" => out.push(entry.path.to_string_lossy().into_owned()),
            _ => {
                let mut s = String::new();
                let mut chars = arg.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '%' {
                        match chars.next() {
                            Some('%') => s.push('%'),
                            Some('u' | 'U') => s.push_str(urls.first().map(String::as_str).unwrap_or("")),
                            Some('c') => s.push_str(entry.main("Name").unwrap_or("")),
                            Some('k') => s.push_str(&entry.path.to_string_lossy()),
                            _ => {}
                        }
                    } else {
                        s.push(c);
                    }
                }
                out.push(s);
            }
        }
    }
    out
}

/// First desktop file with this ID, `$XDG_DATA_HOME/applications` first.
pub fn find_entry(id: &str) -> Option<PathBuf> {
    paths::application_dirs().into_iter().map(|d| d.join(id)).find(|p| p.is_file())
}

fn desktop_names() -> Vec<String> {
    env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Default handler ID for a MIME type or `x-scheme-handler/<scheme>`, resolved the
/// way GIO and xdg-mime do: directories in order, and within each directory
/// `<desktop>-mimeapps.list` before `mimeapps.list`. The first listed ID whose
/// desktop file exists wins.
pub fn default_for(mime: &str) -> Option<String> {
    let names = desktop_names();
    for dir in paths::mimeapps_dirs() {
        let mut lists: Vec<PathBuf> = names.iter().map(|n| dir.join(format!("{n}-mimeapps.list"))).collect();
        lists.push(dir.join("mimeapps.list"));
        for list in lists {
            let Ok(text) = fs::read_to_string(&list) else { continue };
            let mut in_defaults = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_defaults = line == "[Default Applications]";
                    continue;
                }
                if !in_defaults {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    if key.trim() == mime {
                        for id in value.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                            if find_entry(id).is_some() {
                                return Some(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// KDE's `[General] BrowserApplication` from kdeglobals, as written: a desktop
/// entry ID (`firefox.desktop`, older configs without `.desktop`) or `!command`.
pub fn kde_browser_setting() -> Option<String> {
    let mut files = vec![paths::config_home().join("kdeglobals")];
    files.extend(paths::config_dirs().into_iter().map(|d| d.join("kdeglobals")));
    files.iter().filter_map(|f| fs::read_to_string(f).ok()).find_map(|text| kde_general_value(&text, "BrowserApplication"))
}

fn kde_general_value(text: &str, key: &str) -> Option<String> {
    let mut in_general = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_general = line == "[General]";
        } else if in_general {
            if let Some((k, v)) = line.split_once('=') {
                // KConfig keys may carry flags such as `Key[$e]`.
                if k.trim().split('[').next() == Some(key) {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Default handler ID for links of `scheme`. KIO opens http(s) links with KDE's
/// BrowserApplication when no scheme handler is set, so that counts as the
/// default then (the `!command` form has no entry to intercept).
pub fn default_handler(scheme: &str) -> Option<String> {
    default_for(&format!("x-scheme-handler/{scheme}")).or_else(|| {
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let value = kde_browser_setting().filter(|v| !v.is_empty() && !v.starts_with('!'))?;
        let id = if value.ends_with(".desktop") { value } else { format!("{value}.desktop") };
        find_entry(&id).map(|_| id)
    })
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return p.is_file().then_some(p);
    }
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path).map(|d| d.join(cmd)).find(|p| p.is_file())
    })
}

/// Gecko browsers (Firefox, Zen, …) keep `application.ini` next to the real binary.
pub fn is_gecko(argv0: &str) -> bool {
    which(argv0)
        .and_then(|p| fs::canonicalize(p).ok())
        .and_then(|p| p.parent().map(|d| d.join("application.ini").is_file()))
        .unwrap_or(false)
}

/// Runs an entry's `Exec` for `urls`, detached. `env` replaces the inherited
/// environment when given and non-empty. `replace_process` execs in place.
pub fn launch(
    entry: &DesktopEntry,
    urls: &[String],
    raw_args: Option<&[String]>,
    env: Option<&BTreeMap<String, String>>,
    moz_launcher: Option<&Path>,
    replace_process: bool,
) -> Result<()> {
    let argv = entry.exec_argv()?;
    let args = match raw_args {
        Some(raw) => {
            let mut a = vec![argv[0].clone()];
            a.extend(raw.iter().cloned());
            a
        }
        None => expand_exec(&argv, urls, entry),
    };
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    if let Some(env) = env.filter(|e| !e.is_empty()) {
        cmd.env_clear().envs(env);
    }
    if let (Some(launcher), true) = (moz_launcher, is_gecko(&argv[0])) {
        cmd.env("MOZ_APP_LAUNCHER", launcher);
    }
    if let Some(dir) = entry.main("Path").filter(|d| !d.is_empty()) {
        cmd.current_dir(dir);
    }
    if replace_process {
        let err = cmd.exec();
        return Err(anyhow!("exec {}: {err}", args[0]));
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().with_context(|| format!("spawning {}", args[0]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_quotes_and_escapes() {
        let argv = tokenize_exec(r#"/opt/b\ x "a b" "q\"t" plain %U"#).unwrap();
        assert_eq!(argv, vec![r"/opt/b\", "x", "a b", "q\"t", "plain", "%U"]);
    }

    #[test]
    fn expands_field_codes() {
        let e = DesktopEntry::parse("/x.desktop".into(), "[Desktop Entry]\nName=B\nIcon=b\n".into());
        let urls = vec!["https://a/".to_string(), "file:///tmp/f.txt".to_string()];
        let out = expand_exec(&["b".into(), "%U".into(), "%i".into(), "%%".into()], &urls, &e);
        assert_eq!(out, vec!["b", "https://a/", "file:///tmp/f.txt", "--icon", "b", "%"]);
        let out = expand_exec(&["b".into(), "%F".into()], &urls, &e);
        assert_eq!(out, vec!["b", "https://a/", "/tmp/f.txt"]);
    }

    #[test]
    fn reads_kde_general_keys() {
        let text = "[Colors:View]\nBrowserApplication=no\n[General]\nBrowserApplication[$e]=firefox.desktop\n";
        assert_eq!(kde_general_value(text, "BrowserApplication").as_deref(), Some("firefox.desktop"));
        assert_eq!(kde_general_value("[General]\nOther=1\n", "BrowserApplication"), None);
    }

    #[test]
    fn parses_groups() {
        let e = DesktopEntry::parse(
            "/x.desktop".into(),
            "[Desktop Entry]\nExec=a %u\n[Desktop Action new]\nExec=a --new\n".into(),
        );
        assert_eq!(e.main("Exec"), Some("a %u"));
        assert_eq!(e.get("Desktop Action new", "Exec"), Some("a --new"));
    }
}
