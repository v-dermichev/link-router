//! Fallback resolver: `yt-dlp -J`, run by the daemon so its result goes through
//! the same playback path as the direct resolvers.

use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

use super::{Content, Fail};

pub async fn resolve(url: &str, max_res: u32, max_fps: u32) -> Result<Content, Fail> {
    let sort = format!("res:{max_res},fps:{max_fps}");
    let run = Command::new("yt-dlp")
        .args(["-J", "--no-playlist", "--no-warnings", "-f", "bv*+ba/b", "-S", &sort, "--", url])
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(45), run)
        .await
        .map_err(|_| Fail::Transient("yt-dlp timed out".into()))?
        .map_err(|e| Fail::Transient(format!("yt-dlp: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| l.contains("ERROR")).unwrap_or("").trim().to_string();
        return Err(Fail::Definitive(format!("yt-dlp failed: {line}")));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| Fail::Transient(format!("yt-dlp JSON: {e}")))?;
    let headers = |f: &Value| -> Vec<(String, String)> {
        f.get("http_headers")
            .and_then(Value::as_object)
            .map(|h| h.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
            .unwrap_or_default()
    };
    let dim = |f: &Value, k: &str| f.get(k).and_then(Value::as_u64).map(|x| x as u32);
    let title = v.get("title").and_then(Value::as_str).map(String::from);
    if let Some(formats) = v.get("requested_formats").and_then(Value::as_array) {
        let is_video = |f: &&Value| f.get("vcodec").and_then(Value::as_str).map(|c| c != "none").unwrap_or(false);
        let video = formats.iter().find(is_video).ok_or_else(|| Fail::Definitive("yt-dlp chose no video stream".into()))?;
        let audio = formats.iter().find(|f| !is_video(f));
        return Ok(Content {
            video: video.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
            audio: audio.and_then(|a| a.get("url")).and_then(Value::as_str).map(String::from),
            title,
            width: dim(video, "width"),
            height: dim(video, "height"),
            headers: headers(video),
        });
    }
    let url = v.get("url").and_then(Value::as_str).ok_or_else(|| Fail::Definitive("yt-dlp returned no URL".into()))?;
    Ok(Content { video: url.to_string(), audio: None, title, width: dim(&v, "width"), height: dim(&v, "height"), headers: headers(&v) })
}
