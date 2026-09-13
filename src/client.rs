//! The per-link client: decides passthrough vs routing and hands links to the
//! daemon. Kept free of the async runtime and HTTP stack so it starts fast.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::desktop;
use crate::intercept::{self, State, SCHEMES};
use crate::paths;

pub const PROTOCOL_VERSION: u32 = 1;
pub const ACK: u8 = 0x06;
pub const NAK: u8 = 0x15;
const ACK_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub v: u32,
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub fallback_id: Option<String>,
    #[serde(default)]
    pub t0_ns: u64,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn is_intercepted_url(arg: &str) -> bool {
    arg.split_once("://")
        .map(|(scheme, _)| SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)))
        .unwrap_or(false)
}

fn looks_like_uri(arg: &str) -> bool {
    arg.split_once(':')
        .map(|(scheme, _)| !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
        .unwrap_or(false)
}

/// The whole environment of the app the link came from: the daemon may run as a
/// service outside the graphical session, and whatever it starts for the link
/// (player, browser) should see what that app would have passed on.
fn forwarded_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// Replaces this process with the original entry of `id`.
fn exec_original(id: &str, state: &State, urls: &[String], raw_args: Option<&[String]>) -> ! {
    let Some(entry) = intercept::original_entry(id, state) else {
        eprintln!("link-router: no original entry for {id}");
        std::process::exit(1);
    };
    let launcher = intercept::by_id_path(id);
    let err = desktop::launch(&entry, urls, raw_args, None, Some(&launcher), true);
    eprintln!("link-router: {err:?}");
    std::process::exit(1);
}

/// Started as `by-id/<id>` by a shadow entry.
pub fn run_by_id(id: &str, args: Vec<String>) -> ! {
    let t0 = monotonic_ns();
    let state = State::load();
    if args.is_empty() {
        exec_original(id, &state, &[], None);
    }
    if !args.iter().all(|a| looks_like_uri(a)) {
        exec_original(id, &state, &[], Some(&args));
    }
    if !args.iter().all(|a| is_intercepted_url(a)) || !state.enabled || !paths::config_file().is_file() {
        exec_original(id, &state, &args, None);
    }
    let msg = Message { v: PROTOCOL_VERSION, cmd: None, urls: args.clone(), fallback_id: Some(id.to_string()), t0_ns: t0, env: forwarded_env() };
    match send(&msg, state.install_path.as_deref()) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("link-router: daemon unavailable ({e}); opening in the original handler");
            exec_original(id, &state, &args, None);
        }
    }
}

/// `link-router open URL…` from a terminal: the fallback is the current default
/// handler of the first URL's scheme.
pub fn run_open(urls: Vec<String>) -> Result<()> {
    let t0 = monotonic_ns();
    let state = State::load();
    let scheme = urls.first().and_then(|u| u.split_once(':')).map(|(s, _)| s.to_lowercase()).ok_or_else(|| anyhow!("no URL"))?;
    let fallback_id = desktop::default_handler(&scheme);
    let msg = Message { v: PROTOCOL_VERSION, cmd: None, urls: urls.clone(), fallback_id: fallback_id.clone(), t0_ns: t0, env: forwarded_env() };
    let install = state.install_path.clone().or_else(|| std::env::current_exe().ok());
    match send(&msg, install.as_deref()) {
        Ok(()) => Ok(()),
        Err(e) => match fallback_id {
            Some(id) => exec_original(&id, &state, &urls, None),
            None => Err(e),
        },
    }
}

fn connect() -> Option<UnixStream> {
    UnixStream::connect(paths::socket()).ok()
}

pub fn daemon_alive() -> bool {
    connect().is_some()
}

fn spawn_daemon(install: Option<&Path>) -> Result<std::process::Child> {
    let exe: PathBuf = match install {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe()?,
    };
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    Ok(cmd.spawn()?)
}

fn try_send(stream: &mut UnixStream, msg: &Message) -> Result<u8> {
    stream.set_write_timeout(Some(ACK_TIMEOUT))?;
    stream.set_read_timeout(Some(ACK_TIMEOUT))?;
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut b = [0u8; 1];
    stream.read_exact(&mut b)?;
    Ok(b[0])
}

pub fn send(msg: &Message, install: Option<&Path>) -> Result<()> {
    let deadline = Instant::now() + ACK_TIMEOUT;
    let mut restarted = false;
    loop {
        let stream = match connect() {
            Some(s) => Some(s),
            None => {
                let mut child = spawn_daemon(install)?;
                let mut s = None;
                while Instant::now() < deadline {
                    if let Some(c) = connect() {
                        s = Some(c);
                        break;
                    }
                    if let Ok(Some(status)) = child.try_wait() {
                        // A second daemon that lost the race exits 0 once the first one listens.
                        if let Some(c) = connect() {
                            s = Some(c);
                            break;
                        }
                        return Err(anyhow!("daemon exited during startup ({status})"));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                s
            }
        };
        let mut stream = stream.ok_or_else(|| anyhow!("daemon did not start in time"))?;
        match try_send(&mut stream, msg)? {
            ACK => return Ok(()),
            NAK if !restarted => {
                // Version mismatch: the daemon exits after answering; start ours.
                restarted = true;
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            other => return Err(anyhow!("unexpected daemon reply {other:#x}")),
        }
    }
}

pub fn shutdown_daemon() {
    if let Some(mut s) = connect() {
        let msg = Message { v: PROTOCOL_VERSION, cmd: Some("shutdown".into()), urls: vec![], fallback_id: None, t0_ns: 0, env: BTreeMap::new() };
        let _ = try_send(&mut s, &msg);
    }
}
