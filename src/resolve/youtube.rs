//! YouTube: one player API request as the `visionos` client, whose adaptive
//! formats carry plain URLs (no signature cipher, no `n` challenge, no PO token).

use serde_json::{json, Value};
use std::fs;
use tokio::sync::Mutex;

use super::select::{self, AudioFormat, VideoFormat};
use super::{Content, Fail};
use crate::paths;

const VISIONOS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const WEB_CLIENT_VERSION: &str = "2.20260708.00.00";
const FIELD_MASK: &str = "playabilityStatus.status,playabilityStatus.reason,videoDetails.videoId,videoDetails.title,videoDetails.isLive,videoDetails.isUpcoming,responseContext.visitorData,streamingData.adaptiveFormats.url,streamingData.adaptiveFormats.mimeType,streamingData.adaptiveFormats.width,streamingData.adaptiveFormats.height,streamingData.adaptiveFormats.fps,streamingData.adaptiveFormats.bitrate,streamingData.adaptiveFormats.isDrc,streamingData.adaptiveFormats.audioTrack,streamingData.adaptiveFormats.type,streamingData.adaptiveFormats.targetDurationSec";

pub struct YouTube {
    visitor: Mutex<Option<String>>,
}

pub fn video_id(url: &url::Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path();
    let id = if host == "youtu.be" {
        path.trim_start_matches('/').split('/').next().map(String::from)
    } else if path == "/watch" {
        url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned())
    } else {
        ["/shorts/", "/live/", "/embed/"]
            .iter()
            .find_map(|p| path.strip_prefix(p))
            .and_then(|rest| rest.split('/').next())
            .map(String::from)
    }?;
    (id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then_some(id)
}

fn visitor_file() -> std::path::PathBuf {
    paths::cache_dir().join("visitor_data")
}

impl YouTube {
    pub fn new() -> Self {
        let cached = fs::read_to_string(visitor_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Self { visitor: Mutex::new(cached) }
    }

    async fn store_visitor(&self, data: &str) {
        let mut guard = self.visitor.lock().await;
        if guard.as_deref() != Some(data) {
            *guard = Some(data.to_string());
            let path = visitor_file();
            let _ = fs::create_dir_all(path.parent().unwrap());
            let _ = fs::write(path, data);
        }
    }

    async fn visitor_data(&self, http: &reqwest::Client) -> Result<String, Fail> {
        if let Some(v) = self.visitor.lock().await.clone() {
            return Ok(v);
        }
        let body = json!({ "context": { "client": { "clientName": "WEB", "clientVersion": WEB_CLIENT_VERSION, "hl": "en" } } });
        let resp: Value = http
            .post("https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false")
            .header("X-YouTube-Client-Name", "1")
            .header("X-YouTube-Client-Version", WEB_CLIENT_VERSION)
            .header("Origin", "https://www.youtube.com")
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        let data = resp
            .pointer("/responseContext/visitorData")
            .and_then(Value::as_str)
            .ok_or_else(|| Fail::Transient("visitor_id response without visitorData".into()))?
            .to_string();
        self.store_visitor(&data).await;
        Ok(data)
    }

    pub async fn resolve(&self, http: &reqwest::Client, id: &str, max_res: u32, max_fps: u32) -> Result<Content, Fail> {
        let visitor = self.visitor_data(http).await?;
        let body = json!({
            "context": { "client": {
                "clientName": "VISIONOS", "clientVersion": "1.02", "deviceMake": "Apple",
                "deviceModel": "RealityDevice17,1", "userAgent": VISIONOS_UA, "osName": "visionOS",
                "osVersion": "26.5.23O471", "hl": "en", "timeZone": "UTC", "utcOffsetMinutes": 0,
                "visitorData": visitor,
            }},
            "videoId": id,
            "playbackContext": { "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" } },
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let resp = http
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("User-Agent", VISIONOS_UA)
            .header("X-YouTube-Client-Name", "101")
            .header("X-YouTube-Client-Version", "1.02")
            .header("X-Goog-Visitor-Id", &visitor)
            .header("X-Goog-FieldMask", FIELD_MASK)
            .header("Origin", "https://www.youtube.com")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Fail::Transient(format!("player API HTTP {}", resp.status())));
        }
        let v: Value = resp.json().await?;
        if let Some(rotated) = v.pointer("/responseContext/visitorData").and_then(Value::as_str) {
            self.store_visitor(rotated).await;
        }
        let status = v.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("");
        if status != "OK" {
            let reason = v.pointer("/playabilityStatus/reason").and_then(Value::as_str).unwrap_or("");
            return Err(Fail::Definitive(format!("playability {status}: {reason}")));
        }
        if v.pointer("/videoDetails/videoId").and_then(Value::as_str) != Some(id) {
            return Err(Fail::Transient("response is for another video".into()));
        }
        let flag = |p: &str| v.pointer(p).and_then(Value::as_bool).unwrap_or(false);
        if flag("/videoDetails/isLive") || flag("/videoDetails/isUpcoming") {
            return Err(Fail::Transient("live or upcoming".into()));
        }
        let formats = v.pointer("/streamingData/adaptiveFormats").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut videos = Vec::new();
        let mut audios = Vec::new();
        for f in &formats {
            let Some(url) = f.get("url").and_then(Value::as_str) else { continue };
            if f.get("type").and_then(Value::as_str) == Some("FORMAT_STREAM_TYPE_OTF") || f.get("targetDurationSec").is_some() {
                continue;
            }
            let mime = f.get("mimeType").and_then(Value::as_str).unwrap_or("");
            let num = |k: &str| f.get(k).and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
            if mime.starts_with("video/") {
                videos.push(VideoFormat {
                    url: url.into(),
                    width: num("width") as u32,
                    height: num("height") as u32,
                    fps: num("fps") as u32,
                    codec_rank: select::codec_rank(mime),
                    bitrate: num("bitrate"),
                });
            } else if mime.starts_with("audio/") {
                let default_track = f.pointer("/audioTrack/audioIsDefault").and_then(Value::as_bool).unwrap_or(true);
                audios.push(AudioFormat {
                    url: url.into(),
                    is_default_track: default_track,
                    is_drc: f.get("isDrc").and_then(Value::as_bool).unwrap_or(false),
                    opus: mime.contains("opus"),
                    bitrate: num("bitrate"),
                });
            }
        }
        let video = select::pick_video(&videos, max_res, max_fps).ok_or_else(|| Fail::Transient("no playable video format".into()))?;
        let audio = select::pick_audio(&audios).ok_or_else(|| Fail::Transient("no playable audio format".into()))?;
        Ok(Content {
            video: video.url.clone(),
            audio: Some(audio.url.clone()),
            title: v.pointer("/videoDetails/title").and_then(Value::as_str).map(String::from),
            width: Some(video.width),
            height: Some(video.height),
            headers: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ids() {
        let id = |u: &str| video_id(&url::Url::parse(u).unwrap());
        assert_eq!(id("https://www.youtube.com/watch?v=jNQXAC9IVRw&t=1"), Some("jNQXAC9IVRw".into()));
        assert_eq!(id("https://youtu.be/jNQXAC9IVRw?si=x"), Some("jNQXAC9IVRw".into()));
        assert_eq!(id("https://youtube.com/shorts/j2Ky4AJ1OvA?si=89"), Some("j2Ky4AJ1OvA".into()));
        assert_eq!(id("https://www.youtube.com/watch?v=short"), None);
    }
}
