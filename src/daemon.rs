//! The daemon: accepts links from clients, runs the mpv-video plugin's resolver
//! chains, drives the player, falls back to the original handler, and keeps
//! interception intact.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::client::{self, Message, ACK, NAK, PROTOCOL_VERSION};
use crate::config::{Config, VideoConfig};
use crate::desktop;
use crate::hypr::{self, Hypr};
use crate::kwin;
use crate::sway::Sway;
use crate::intercept::{self, State, UserOverride};
use crate::mpv::{self, Outcome, Player};
use crate::paths;
use crate::resolve::{self, instagram, youtube, ytdlp, Content, Fail, Site};

struct Daemon {
    config: Config,
    http: reqwest::Client,
    http_no_redirect: reqwest::Client,
    youtube: youtube::YouTube,
    player: Mutex<Option<Arc<Player>>>,
    in_flight: AtomicUsize,
    last_activity: StdMutex<Instant>,
    log: StdMutex<Option<std::fs::File>>,
}

impl Daemon {
    fn log(&self, t0_ns: u64, line: &str) {
        let ms = if t0_ns > 0 { (client::monotonic_ns().saturating_sub(t0_ns)) as f64 / 1e6 } else { 0.0 };
        if let Some(f) = self.log.lock().unwrap().as_mut() {
            let _ = writeln!(f, "[{ms:9.1} ms] {line}");
        }
    }

    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }
}

/// `resident` (for service managers) disables the idle exit.
pub async fn run(resident: bool) -> Result<()> {
    let socket = paths::socket();
    std::fs::create_dir_all(paths::runtime_dir())?;
    std::fs::set_permissions(paths::runtime_dir(), std::fs::Permissions::from_mode(0o700))?;
    if UnixStream::connect(&socket).await.is_ok() {
        if !resident {
            return Ok(());
        }
        // The service instance takes over from one started on demand by a click.
        tokio::task::spawn_blocking(client::shutdown_daemon).await?;
        for _ in 0..100 {
            if UnixStream::connect(&socket).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).context("binding daemon socket")?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    std::fs::create_dir_all(paths::state_dir())?;
    let log = OpenOptions::new().create(true).append(true).open(paths::log_file()).ok();
    let config = Config::load();
    let d = Arc::new(Daemon {
        http: reqwest::Client::builder()
            .use_rustls_tls()
            .pool_idle_timeout(Duration::from_secs(230))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?,
        http_no_redirect: reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .pool_idle_timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?,
        youtube: youtube::YouTube::new(),
        player: Mutex::new(None),
        in_flight: AtomicUsize::new(0),
        last_activity: StdMutex::new(Instant::now()),
        log: StdMutex::new(log),
        config,
    });
    d.log(0, &format!("daemon started (pid {}), mpv-video {}", std::process::id(), if d.config.video.enabled { "enabled" } else { "disabled" }));
    for w in &d.config.warnings {
        d.log(0, &format!("config: {w}"));
    }

    run_sentinel(&d);
    spawn_watcher(d.clone());
    if !resident {
        spawn_idle_exit(d.clone());
    }

    loop {
        let (stream, _) = listener.accept().await?;
        let d = d.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(d.clone(), stream).await {
                d.log(0, &format!("connection error: {e:#}"));
            }
        });
    }
}

async fn handle_connection(d: Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    if line.is_empty() {
        // A liveness probe: connected and closed without a message.
        return Ok(());
    }
    let msg: Message = serde_json::from_str(&line)?;
    if msg.v != PROTOCOL_VERSION {
        write.write_all(&[NAK]).await?;
        d.log(0, &format!("client protocol {} != {PROTOCOL_VERSION}; exiting for restart", msg.v));
        std::process::exit(0);
    }
    write.write_all(&[ACK]).await?;
    drop(write);
    if msg.cmd.as_deref() == Some("shutdown") {
        if let Some(p) = d.player.lock().await.take() {
            p.quit().await;
        }
        d.log(0, "shutdown requested");
        let _ = std::fs::remove_file(paths::socket());
        std::process::exit(0);
    }
    d.touch();
    d.in_flight.fetch_add(1, Ordering::SeqCst);
    for url in &msg.urls {
        handle_url(&d, &msg, url).await;
    }
    d.in_flight.fetch_sub(1, Ordering::SeqCst);
    d.touch();
    Ok(())
}

enum Step {
    Direct,
    YouTubeDirect(String),
    InstagramDirect,
    YtDlp,
}

async fn handle_url(d: &Arc<Daemon>, msg: &Message, raw: &str) {
    let t0 = msg.t0_ns;
    d.log(t0, &format!("link {raw} (via {})", msg.fallback_id.as_deref().unwrap_or("-")));
    let video = &d.config.video;
    let parsed = url::Url::parse(raw).ok();
    let site = if video.enabled { parsed.as_ref().and_then(resolve::site_of) } else { None };
    let steps: Vec<Step> = match site {
        Some(Site::YouTube) if video.youtube => match parsed.as_ref().and_then(youtube::video_id) {
            Some(id) => vec![Step::YouTubeDirect(id), Step::YtDlp],
            None => vec![Step::YtDlp],
        },
        Some(Site::Instagram) if video.instagram => vec![Step::InstagramDirect, Step::YtDlp],
        _ if video.enabled && parsed.as_ref().is_some_and(|u| resolve::is_direct_media(u, &video.direct)) => vec![Step::Direct],
        _ => Vec::new(),
    };
    for step in steps {
        let name = match &step {
            Step::Direct => "direct",
            Step::YouTubeDirect(_) => "youtube_direct",
            Step::InstagramDirect => "instagram_direct",
            Step::YtDlp => "ytdlp",
        };
        let result = match &step {
            Step::Direct => Ok(Content { video: raw.to_string(), ..Default::default() }),
            Step::YouTubeDirect(id) => d.youtube.resolve(&d.http, id, video.max_resolution, video.max_fps).await,
            Step::InstagramDirect => instagram::resolve(&d.http_no_redirect, parsed.as_ref().unwrap(), video.max_resolution, video.max_fps).await,
            Step::YtDlp => ytdlp::resolve(raw, video.max_resolution, video.max_fps).await,
        };
        match result {
            Ok(content) => {
                d.log(t0, &format!("{name}: resolved {}x{}", content.width.unwrap_or(0), content.height.unwrap_or(0)));
                match play(d, msg, &content).await {
                    Ok(Played::Playing) => {
                        d.log(t0, "playing");
                        return;
                    }
                    Ok(Played::Superseded) => {
                        d.log(t0, "superseded by a newer link");
                        return;
                    }
                    Ok(Played::StreamFailed(reason)) => d.log(t0, &format!("{name}: playback failed: {reason}")),
                    Err(e) => {
                        d.log(t0, &format!("{name}: player failed: {e:#}"));
                        break;
                    }
                }
            }
            Err(e @ (Fail::NotVideo(_) | Fail::Definitive(_))) => {
                d.log(t0, &format!("{name}: {e}"));
                break;
            }
            Err(e) => d.log(t0, &format!("{name}: {e}")),
        }
    }
    open_fallback(d, msg, raw);
}

/// Plays `content`; `Ok(false)` when a newer link replaced it before it started.
enum Played {
    Playing,
    Superseded,
    /// The player works but this stream didn't play; another resolver may do better.
    StreamFailed(String),
}

/// How the player window gets placed for the session a link came from.
enum Placer {
    /// A runtime window rule for a new window, dispatches for a running one.
    Hyprland(Hypr, hypr::Monitor),
    /// A `for_window` rule on the new player's PID, set while mpv waits in the
    /// size handshake before creating its window; commands for a running one.
    Sway(Sway, hypr::Monitor),
    /// mpv sizes the window from `geometry`; KWin's placement script positions it.
    KWin,
    /// mpv's `geometry` with bottom-right offsets (position honoured on X11 only).
    Plain,
    /// A running player the user made fullscreen: left alone.
    Fullscreen,
}

async fn placer_for(d: &Daemon, env: &std::collections::BTreeMap<String, String>, spawning: bool) -> Placer {
    if let Some(h) = Hypr::from_env(env) {
        if let Ok(m) = h.focused_monitor().await {
            return Placer::Hyprland(h, m);
        }
    }
    if let Some(s) = Sway::from_env(env) {
        match s.focused_workspace().await {
            Ok(m) => return Placer::Sway(s, m),
            Err(e) => d.log(0, &format!("sway IPC unavailable: {e:#}")),
        }
    }
    if kwin::is_kde(env) {
        if !spawning {
            return Placer::KWin;
        }
        match kwin::ensure_script(&d.config.video, env).await {
            Ok(()) => return Placer::KWin,
            Err(e) => d.log(0, &format!("KWin placement script unavailable: {e:#}")),
        }
    }
    Placer::Plain
}

/// Sizes and positions the player window for `size`. `player` is `None` before
/// a new player is started (the size then goes on its command line), and
/// `fresh` means its window isn't mapped yet.
async fn place(cfg: &VideoConfig, placer: &Placer, player: Option<&Player>, fresh: bool, size: (u32, u32)) -> Result<()> {
    match (placer, player) {
        (Placer::Hyprland(h, m), _) if fresh => h.set_rule(&cfg.app_id, size, hypr::anchor(m, size, cfg.margin).0).await,
        (Placer::Hyprland(h, m), Some(p)) => {
            if let Some(c) = h.client(&cfg.app_id, p.pid).await? {
                h.resize_move(&c.address, size, hypr::anchor(m, size, cfg.margin).1).await?;
            }
            Ok(())
        }
        (Placer::Fullscreen, _) => Ok(()),
        (Placer::Sway(s, m), Some(p @ Player { pid: Some(pid), .. })) => {
            // sway lets mpv resize its floating window for each new video, so mpv
            // gets the size as well.
            p.command(serde_json::json!(["set_property", "geometry", mpv::geometry(size, cfg.margin)])).await?;
            let absolute = hypr::anchor(m, size, cfg.margin).1;
            if fresh {
                s.rule_for_pid(*pid, size, absolute).await
            } else {
                s.resize_move(*pid, size, absolute).await
            }
        }
        (_, Some(p)) => p
            .command(serde_json::json!(["set_property", "geometry", mpv::geometry(size, cfg.margin)]))
            .await
            .map(|_| ()),
        _ => Ok(()),
    }
}

/// Errors are local player failures, which no other resolver can fix.
async fn play(d: &Arc<Daemon>, msg: &Message, content: &Content) -> Result<Played> {
    let cfg = &d.config.video;
    let known = match (content.width, content.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some(hypr::fit((w, h), cfg.box_size)),
        _ => None,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(cfg.network_timeout as u64 + 12);

    let mut guard = d.player.lock().await;
    let running = guard.as_ref().filter(|p| p.alive()).cloned();
    let mut placer = placer_for(d, &msg.env, running.is_none()).await;
    let mut existing = None;
    if let Some(p) = running {
        // A fullscreen player stays as the user set it; only the file changes.
        let fullscreen = p.command(serde_json::json!(["get_property", "fullscreen"])).await.ok().and_then(|v| v.as_bool()) == Some(true);
        if fullscreen {
            placer = Placer::Fullscreen;
        }
        if cfg.follow_focus {
            match (&placer, p.pid) {
                (Placer::Hyprland(h, m), _) => {
                    if let Ok(Some(c)) = h.client(&cfg.app_id, p.pid).await {
                        let _ = h.move_to_workspace(&c.address, &m.workspace).await;
                    }
                }
                (Placer::Sway(s, m), Some(pid)) => {
                    let _ = s.move_to_workspace(pid, &m.workspace).await;
                }
                _ => {}
            }
        }
        if let Some(size) = known {
            let _ = place(cfg, &placer, Some(&p), false, size).await;
        }
        let rx = p.subscribe();
        match p.load(content, cfg, known.is_none()).await {
            Ok(entry) => existing = Some((p, rx, entry)),
            // Typically a player quitting on idle just as the link arrived.
            Err(e) => {
                d.log(msg.t0_ns, &format!("running player unusable ({e:#}), starting a new one"));
                placer = placer_for(d, &msg.env, true).await;
            }
        }
    }
    let (player, mut rx, entry, fresh) = match existing {
        Some((p, rx, entry)) => (p, rx, entry, false),
        None => {
            let mut geometry = None;
            if let Some(size) = known {
                match placer {
                    Placer::Hyprland(..) => place(cfg, &placer, None, true, size).await?,
                    _ => geometry = Some(size),
                }
            }
            // sway can only place a window by PID, which exists once mpv runs: mpv
            // then always reports its size and waits for the rule.
            let handshake = known.is_none() || matches!(placer, Placer::Sway(..));
            let (p, rx) = Player::spawn(cfg, content, &msg.env, geometry, handshake).await?;
            *guard = Some(p.clone());
            spawn_idle_quit(d.clone(), p.clone());
            (p, rx, None, true)
        }
    };
    drop(guard);
    match known {
        Some(size) => d.log(msg.t0_ns, &format!("loading ({}x{} window)", size.0, size.1)),
        None => d.log(msg.t0_ns, "loading (window sized once the stream is open)"),
    }
    loop {
        match mpv::wait_outcome(&mut rx, entry, deadline).await {
            Outcome::Size { seq, width, height } => {
                let size = hypr::fit((width, height), cfg.box_size);
                if let Err(e) = place(cfg, &placer, Some(player.as_ref()), fresh, size).await {
                    d.log(msg.t0_ns, &format!("placing window: {e:#}"));
                }
                player.placed(seq).await;
                d.log(msg.t0_ns, &format!("video {width}x{height}, {}x{} window", size.0, size.1));
            }
            Outcome::Playing => return Ok(Played::Playing),
            Outcome::Superseded => return Ok(Played::Superseded),
            Outcome::Failed(reason) => return Ok(Played::StreamFailed(reason)),
        }
    }
}

fn open_fallback(d: &Arc<Daemon>, msg: &Message, url: &str) {
    let state = State::load();
    let id = msg.fallback_id.clone().or_else(|| {
        let scheme = url.split_once(':').map(|(s, _)| s.to_lowercase())?;
        desktop::default_handler(&scheme)
    });
    let Some(id) = id else {
        d.log(msg.t0_ns, "no fallback handler");
        return;
    };
    let Some(entry) = intercept::original_entry(&id, &state) else {
        d.log(msg.t0_ns, &format!("no original entry for {id}"));
        return;
    };
    let launcher = intercept::by_id_path(&id);
    match desktop::launch(&entry, &[url.to_string()], None, Some(&msg.env), Some(&launcher), false) {
        Ok(()) => d.log(msg.t0_ns, &format!("opened in {id}")),
        Err(e) => d.log(msg.t0_ns, &format!("fallback {id} failed: {e:#}")),
    }
}

/// Quits the player once it is idle with no link in progress.
fn spawn_idle_quit(d: Arc<Daemon>, player: Arc<Player>) {
    let mut rx = player.subscribe();
    tokio::spawn(async move {
        loop {
            let Ok(ev) = rx.recv().await else { break };
            let name = ev.get("event").and_then(Value::as_str).unwrap_or("");
            if name == "link-router-disconnected" {
                break;
            }
            let idle = name == "property-change"
                && ev.get("id").and_then(Value::as_u64) == Some(1)
                && ev.get("data").and_then(Value::as_bool) == Some(true);
            if idle {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if d.in_flight.load(Ordering::SeqCst) == 0 {
                    if let Ok(v) = player.command(serde_json::json!(["get_property", "idle-active"])).await {
                        if v.as_bool() == Some(true) {
                            player.quit().await;
                            break;
                        }
                    }
                }
            }
        }
    });
}

fn spawn_idle_exit(d: Arc<Daemon>) {
    let Some(limit) = d.config.idle_exit else { return };
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let player_alive = d.player.lock().await.as_ref().map(|p| p.alive()).unwrap_or(false);
            let idle_for = d.last_activity.lock().unwrap().elapsed();
            if !player_alive && d.in_flight.load(Ordering::SeqCst) == 0 && idle_for >= limit {
                d.log(0, "idle exit");
                let _ = std::fs::remove_file(paths::socket());
                std::process::exit(0);
            }
        }
    });
}

fn run_sentinel(d: &Arc<Daemon>) {
    let mut state = State::load();
    match intercept::sentinel(&mut state, UserOverride::Leave) {
        Ok(lines) => {
            for l in lines {
                d.log(0, &format!("sentinel: {l}"));
            }
        }
        Err(e) => d.log(0, &format!("sentinel error: {e:#}")),
    }
}

/// inotify on every directory holding mimeapps lists or desktop entries; a
/// burst of changes runs one sentinel pass after 500 ms of quiet.
#[cfg(target_os = "linux")]
mod inotify {
    pub use libc::{inotify_add_watch, inotify_init1, IN_CLOEXEC, IN_CLOSE_WRITE, IN_CREATE, IN_DELETE, IN_MOVED_FROM, IN_MOVED_TO};
}

/// FreeBSD 15 has inotify(2) with Linux's event bits; the libc crate doesn't bind it yet.
#[cfg(target_os = "freebsd")]
mod inotify {
    use libc::{c_char, c_int};
    extern "C" {
        pub fn inotify_init1(flags: c_int) -> c_int;
        pub fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int;
    }
    pub const IN_CLOEXEC: c_int = libc::O_CLOEXEC;
    pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
    pub const IN_MOVED_FROM: u32 = 0x0000_0040;
    pub const IN_MOVED_TO: u32 = 0x0000_0080;
    pub const IN_CREATE: u32 = 0x0000_0100;
    pub const IN_DELETE: u32 = 0x0000_0200;
}

fn spawn_watcher(d: Arc<Daemon>) {
    std::thread::spawn(move || {
        let fd = unsafe { inotify::inotify_init1(inotify::IN_CLOEXEC) };
        if fd < 0 {
            d.log(0, "inotify unavailable; sentinel runs only at start");
            return;
        }
        let mask = inotify::IN_CLOSE_WRITE | inotify::IN_MOVED_TO | inotify::IN_CREATE | inotify::IN_DELETE | inotify::IN_MOVED_FROM;
        let mut dirs = paths::mimeapps_dirs();
        dirs.extend(paths::application_dirs());
        dirs.sort();
        dirs.dedup();
        for dir in dirs.iter().filter(|p| p.is_dir()) {
            if let Ok(c) = std::ffi::CString::new(dir.to_string_lossy().as_bytes()) {
                unsafe { inotify::inotify_add_watch(fd, c.as_ptr(), mask) };
            }
        }
        let mut buf = [0u8; 8192];
        let mut last_pass_end = Instant::now() - Duration::from_secs(10);
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            let own_write = last_pass_end.elapsed() < Duration::from_millis(1500);
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            while unsafe { libc::poll(&mut pfd, 1, 500) } > 0 {
                unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            }
            if own_write {
                continue;
            }
            run_sentinel(&d);
            last_pass_end = Instant::now();
        }
    });
}

/// `link-router resolve URL…`: runs the resolver chain and prints the result.
pub async fn resolve_only(urls: Vec<String>) -> Result<()> {
    let config = Config::load();
    let v = &config.video;
    let http = reqwest::Client::builder().use_rustls_tls().timeout(Duration::from_secs(15)).build()?;
    let http_no_redirect = reqwest::Client::builder().use_rustls_tls().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(15)).build()?;
    let yt = youtube::YouTube::new();
    for raw in urls {
        let parsed = url::Url::parse(&raw)?;
        let started = Instant::now();
        let result = match resolve::site_of(&parsed) {
            Some(Site::YouTube) => match youtube::video_id(&parsed) {
                Some(id) => yt.resolve(&http, &id, v.max_resolution, v.max_fps).await,
                None => ytdlp::resolve(&raw, v.max_resolution, v.max_fps).await,
            },
            Some(Site::Instagram) => instagram::resolve(&http_no_redirect, &parsed, v.max_resolution, v.max_fps).await,
            None => ytdlp::resolve(&raw, v.max_resolution, v.max_fps).await,
        };
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(c) => {
                let host = |u: &str| url::Url::parse(u).ok().and_then(|p| p.host_str().map(String::from)).unwrap_or_default();
                println!(
                    "{raw}\n  {ms:.0} ms  {}x{}  title={:?}\n  video host {}  audio {}",
                    c.width.unwrap_or(0),
                    c.height.unwrap_or(0),
                    c.title,
                    host(&c.video),
                    c.audio.as_deref().map(host).unwrap_or_else(|| "in video stream".into())
                );
            }
            Err(e) => println!("{raw}\n  {ms:.0} ms  {e}"),
        }
    }
    Ok(())
}

