//! mpv driver: one player per daemon, controlled over a persistent JSON IPC
//! connection.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{broadcast, oneshot};

use crate::config::VideoConfig;
use crate::paths;
use crate::resolve::Content;

/// Adds the separate audio stream as soon as the file starts loading, so video
/// and audio open in parallel, and holds playback start until the track is
/// attached (otherwise the first part of the audio is skipped).
///
/// When the daemon doesn't know the video size (direct links, some yt-dlp
/// results), the script publishes the demuxed size at `on_preloaded`, before
/// mpv creates or reconfigures the window, and holds playback until the daemon
/// has placed the window and acknowledged that request.
const SCRIPT: &str = r#"
local first_audio = mp.get_opt("link-router-audio")
local first_place = mp.get_opt("link-router-place") == "yes"
local want_place = false
local audio_pending, place_pending = false, false
local seq, deferred = 0, nil

local function continue_hook(force)
    if deferred and (force or not (audio_pending or place_pending)) then
        local h = deferred
        deferred = nil
        h:cont()
    end
end

mp.register_event("start-file", function()
    local url = mp.get_property_native("user-data/link-router/audio")
    if url and url ~= "" then
        mp.set_property_native("user-data/link-router/audio", "")
    else
        url, first_audio = first_audio, nil
    end
    want_place = mp.get_property_native("user-data/link-router/place") == "yes" or first_place
    first_place = false
    mp.set_property_native("user-data/link-router/place", "")
    mp.set_property_native("user-data/link-router/audio-error", "")
    audio_pending, place_pending = false, false
    if not url or url == "" then return end
    audio_pending = true
    mp.command_native_async({ name = "audio-add", url = url, flags = "select" }, function(ok, _, err)
        audio_pending = false
        if not ok then
            mp.set_property_native("user-data/link-router/audio-error", tostring(err or "audio-add failed"))
        end
        continue_hook(false)
    end)
end)

mp.observe_property("user-data/link-router/placed", "native", function(_, v)
    if place_pending and tonumber(v) == seq then
        place_pending = false
        continue_hook(false)
    end
end)

-- Tracks aren't selected yet at on_preloaded: take the default video track,
-- else the first one, skipping cover art.
local function video_size()
    local tracks = {}
    for _, t in ipairs(mp.get_property_native("track-list") or {}) do
        if t.type == "video" and not t.image and not t.albumart then
            table.insert(tracks, t.default and 1 or #tracks + 1, t)
        end
    end
    for _, t in ipairs(tracks) do
        if (t["demux-w"] or 0) > 0 then
            local w, h = t["demux-w"], t["demux-h"] or 0
            if (t["demux-par"] or 0) > 0 then w = math.floor(w * t["demux-par"] + 0.5) end
            if (t["demux-rotation"] or 0) % 180 == 90 then w, h = h, w end
            if h > 0 then return w, h end
        end
    end
end

mp.add_hook("on_preloaded", 50, function(hook)
    if want_place then
        want_place = false
        local w, h = video_size()
        if w then
            seq = seq + 1
            place_pending = true
            mp.set_property_native("user-data/link-router/size", seq .. " " .. w .. " " .. h)
        end
    end
    if not (audio_pending or place_pending) then return end
    deferred = hook
    hook:defer()
    local token = seq
    mp.add_timeout(2, function()
        if deferred == hook and token == seq then
            audio_pending, place_pending = false, false
            continue_hook(true)
        end
    end)
end)
"#;

pub enum Outcome {
    /// The demuxed size of a file loaded with `place`; answer with [`Player::placed`].
    Size { seq: u64, width: u32, height: u32 },
    Playing,
    Superseded,
    Failed(String),
}

pub struct Player {
    pub pid: Option<u32>,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    events: broadcast::Sender<Value>,
    alive: Arc<AtomicBool>,
}

fn script_path() -> Result<PathBuf> {
    let dir = paths::runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mpv-link-router.lua");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SCRIPT) {
        std::fs::write(&path, SCRIPT)?;
    }
    Ok(path)
}

fn per_file_options(content: &Content, cfg: &VideoConfig) -> serde_json::Map<String, Value> {
    let mut o = serde_json::Map::new();
    if let Some(t) = &content.title {
        o.insert("force-media-title".into(), json!(t));
    }
    if !content.headers.is_empty() {
        // A string list option: `,` separates items unless escaped.
        let fields: Vec<String> = content
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}").replace('\\', "\\\\").replace(',', "\\,"))
            .collect();
        o.insert("http-header-fields".into(), json!(fields.join(",")));
    }
    o.insert("network-timeout".into(), json!(cfg.network_timeout.to_string()));
    o
}

/// mpv `geometry` for a window of `size` anchored bottom-right with `margin`
/// (`-x` counts from the right edge, `--x` beyond it). Wayland compositors
/// ignore the position; X11 window managers use it.
pub fn geometry((w, h): (u32, u32), (mx, my): (i32, i32)) -> String {
    format!("{w}x{h}-{mx}-{my}")
}

impl Player {
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Starts mpv playing `content`. `geometry` is passed to mpv when no
    /// compositor rule sizes the window.
    pub async fn spawn(
        cfg: &VideoConfig,
        content: &Content,
        link_env: &BTreeMap<String, String>,
        geometry: Option<(u32, u32)>,
        place: bool,
    ) -> Result<(Arc<Player>, broadcast::Receiver<Value>)> {
        // A socket per player, so connecting can't reach an old mpv that is still quitting.
        static SPAWNED: AtomicU64 = AtomicU64::new(0);
        let socket = paths::runtime_dir().join(format!("mpv-{}.sock", SPAWNED.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_file(&socket);
        let script = script_path()?;
        let mut args: Vec<String> = cfg.args.clone();
        args.extend([
            format!("--wayland-app-id={}", cfg.app_id),
            format!("--x11-name={}", cfg.app_id),
            format!("--input-ipc-server={}", socket.display()),
            "--idle=yes".into(),
            "--ytdl=no".into(),
            "--force-window=no".into(),
            "--keep-open=no".into(),
            format!("--script={}", script.display()),
            format!("--cache-pause-initial={}", if cfg.buffer_secs > 0.0 { "yes" } else { "no" }),
            format!("--cache-pause-wait={}", cfg.buffer_secs),
        ]);
        for opt in &cfg.stream_options {
            args.push(format!("--stream-lavf-o-append={opt}"));
        }
        if let Some(audio) = &content.audio {
            args.push(format!("--script-opts-append=link-router-audio={audio}"));
        }
        if place {
            args.push("--script-opts-append=link-router-place=yes".into());
        }
        if let Some(size) = geometry {
            args.push(format!("--geometry={}", self::geometry(size, cfg.margin)));
        }
        args.push("--{".into());
        for (k, v) in per_file_options(content, cfg) {
            if let Some(s) = v.as_str() {
                args.push(format!("--{k}={s}"));
            }
        }
        // No `--` inside the group: it would end option parsing and swallow `--}`.
        args.push(content.video.clone());
        args.push("--}".into());

        let mut cmd = tokio::process::Command::new("mpv");
        let _ = std::fs::create_dir_all(paths::state_dir());
        let stderr = std::fs::File::create(paths::state_dir().join("mpv.log")).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr);
        if !link_env.is_empty() {
            cmd.env_clear().envs(link_env);
        }
        for (k, v) in &cfg.env {
            match v {
                Some(val) => cmd.env(k, val),
                None => cmd.env_remove(k),
            };
        }
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn().context("starting mpv")?;
        let pid = child.id();

        let mut stream = None;
        for _ in 0..600 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                stream = Some(s);
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let log = std::fs::read_to_string(paths::state_dir().join("mpv.log")).unwrap_or_default();
                let last = log.lines().rev().find(|l| !l.trim().is_empty() && !l.starts_with("Exiting")).unwrap_or("");
                return Err(anyhow!("mpv exited during startup ({status}): {last}"));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let stream = stream.ok_or_else(|| anyhow!("mpv IPC socket did not appear"))?;
        let (read, write) = stream.into_split();
        let (events, rx) = broadcast::channel(256);
        let player = Arc::new(Player {
            pid,
            writer: tokio::sync::Mutex::new(write),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            events: events.clone(),
            alive: Arc::new(AtomicBool::new(true)),
        });

        let pending = player.pending.clone();
        let alive = player.alive.clone();
        let socket_path = socket.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(id) = v.get("request_id").and_then(Value::as_u64) {
                    if let Some(tx) = pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(v);
                    }
                } else if v.get("event").is_some() {
                    let _ = events.send(v);
                }
            }
            alive.store(false, Ordering::SeqCst);
            let _ = events.send(json!({ "event": "link-router-disconnected" }));
            let _ = child.wait().await;
            let _ = std::fs::remove_file(&socket_path);
        });

        player.command(json!(["observe_property", 1, "idle-active"])).await.ok();
        player.command(json!(["observe_property", 2, "user-data/link-router/audio-error"])).await.ok();
        player.command(json!(["observe_property", 3, "user-data/link-router/size"])).await.ok();
        Ok((player, rx))
    }

    pub async fn command(&self, args: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut line = serde_json::to_vec(&json!({ "command": args, "request_id": id }))?;
        line.push(b'\n');
        self.writer.lock().await.write_all(&line).await?;
        let reply = tokio::time::timeout(Duration::from_secs(5), rx).await.map_err(|_| anyhow!("mpv did not answer"))??;
        if reply.get("error").and_then(Value::as_str) != Some("success") {
            return Err(anyhow!("mpv: {}", reply));
        }
        Ok(reply.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Replaces the current item with `content`; returns the playlist entry id.
    pub async fn load(&self, content: &Content, cfg: &VideoConfig, place: bool) -> Result<Option<u64>> {
        self.command(json!(["set_property", "user-data/link-router/audio", content.audio.clone().unwrap_or_default()])).await?;
        self.command(json!(["set_property", "user-data/link-router/place", if place { "yes" } else { "" }])).await?;
        let reply = self
            .command(json!(["loadfile", content.video, "replace", -1, Value::Object(per_file_options(content, cfg))]))
            .await?;
        Ok(reply.get("playlist_entry_id").and_then(Value::as_u64))
    }

    /// Lets the file of size request `seq` start playing.
    pub async fn placed(&self, seq: u64) {
        let _ = self.command(json!(["set_property", "user-data/link-router/placed", seq])).await;
    }

    pub async fn quit(&self) {
        let _ = self.command(json!(["quit"])).await;
    }
}

/// Waits for the item `entry` (or the first item when `None`) to start playing
/// or fail.
pub async fn wait_outcome(rx: &mut broadcast::Receiver<Value>, entry: Option<u64>, deadline: tokio::time::Instant) -> Outcome {
    loop {
        let ev = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => return Outcome::Failed("timed out waiting for playback".into()),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Outcome::Failed("player gone".into()),
            Ok(Ok(v)) => v,
        };
        let name = ev.get("event").and_then(Value::as_str).unwrap_or("");
        let matches = |ev: &Value| match (entry, ev.get("playlist_entry_id").and_then(Value::as_u64)) {
            (Some(want), Some(got)) => want == got,
            _ => true,
        };
        match name {
            "playback-restart" => return Outcome::Playing,
            "end-file" if matches(&ev) => {
                let reason = ev.get("reason").and_then(Value::as_str).unwrap_or("");
                match reason {
                    "error" => {
                        let detail = ev.get("file_error").and_then(Value::as_str).unwrap_or("load error");
                        return Outcome::Failed(detail.to_string());
                    }
                    "stop" | "redirect" => return Outcome::Superseded,
                    _ => return Outcome::Failed(format!("ended before playback ({reason})")),
                }
            }
            "property-change" if ev.get("id").and_then(Value::as_u64) == Some(3) => {
                let data = ev.get("data").and_then(Value::as_str).unwrap_or("");
                let nums: Vec<u64> = data.split(' ').filter_map(|x| x.parse().ok()).collect();
                if let [seq, width, height] = nums[..] {
                    return Outcome::Size { seq, width: width as u32, height: height as u32 };
                }
            }
            "property-change" if ev.get("id").and_then(Value::as_u64) == Some(2) => {
                if let Some(err) = ev.get("data").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    return Outcome::Failed(format!("audio: {err}"));
                }
            }
            "link-router-disconnected" => return Outcome::Failed("player exited".into()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_anchors_bottom_right() {
        assert_eq!(geometry((960, 540), (35, 25)), "960x540-35-25");
        assert_eq!(geometry((405, 720), (0, 0)), "405x720-0-0");
    }

    #[test]
    fn escapes_header_list_separators() {
        let content = Content { headers: vec![("User-Agent".into(), r"A (B, C) \x".into())], ..Default::default() };
        let o = per_file_options(&content, &VideoConfig::default());
        assert_eq!(o["http-header-fields"], json!(r"User-Agent: A (B\, C) \\x"));
    }
}
