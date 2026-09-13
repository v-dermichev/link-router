//! Instagram: the logged-out reel page, read only until the post's DASH manifest
//! arrives. The manifest has the full-quality representations; the page's
//! progressive `video_versions` are 480p.

use futures_util::StreamExt;
use quick_xml::events::Event;
use quick_xml::Reader;

use super::select::{self, AudioFormat, VideoFormat};
use super::{Content, Fail};

const DESKTOP_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// `(kind, shortcode)` for `/p/`, `/tv/`, `/reel/`, `/reels/`, optionally under `/<user>/`.
pub fn shortcode(path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let find = |i: usize| -> Option<(String, String)> {
        let kind = *parts.get(i)?;
        let code = *parts.get(i + 1)?;
        matches!(kind, "p" | "tv" | "reel" | "reels").then(|| (kind.to_string(), code.to_string()))
    };
    find(0).or_else(|| find(1))
}

fn find_bytes(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// The JSON string value following `"key":"` at or after `from`, once it is
/// complete in `page`.
fn json_string_after(page: &[u8], key: &str, from: usize) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = find_bytes(page, needle.as_bytes(), from)? + needle.len();
    let mut i = start;
    while i < page.len() {
        match page[i] {
            b'\\' => i += 2,
            b'"' => {
                let raw = String::from_utf8_lossy(&page[start..i]);
                return serde_json::from_str(&format!("\"{raw}\"")).ok();
            }
            _ => i += 1,
        }
    }
    None
}

fn frame_rate(s: &str) -> u32 {
    match s.split_once('/') {
        Some((n, d)) => {
            let (n, d): (f64, f64) = (n.parse().unwrap_or(0.0), d.parse().unwrap_or(1.0));
            if d > 0.0 { (n / d).round() as u32 } else { 0 }
        }
        None => s.parse::<f64>().map(|f| f.round() as u32).unwrap_or(0),
    }
}

/// Video and audio representations of a DASH manifest with single-file `BaseURL`s.
pub fn parse_manifest(xml: &str) -> (Vec<VideoFormat>, Vec<AudioFormat>) {
    struct Rep {
        mime: String,
        width: u32,
        height: u32,
        fps: u32,
        bandwidth: u64,
        codecs: String,
    }
    let mut reader = Reader::from_str(xml);
    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let mut set_kind = String::new();
    let mut rep: Option<Rep> = None;
    let mut base: Option<String> = None;
    let attr = |e: &quick_xml::events::BytesStart, name: &str| -> Option<String> {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == name.as_bytes())
            .and_then(|a| a.unescape_value().ok())
            .map(|v| v.into_owned())
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"AdaptationSet" => {
                    set_kind = attr(&e, "contentType").or_else(|| attr(&e, "mimeType")).unwrap_or_default();
                }
                b"Representation" => {
                    rep = Some(Rep {
                        mime: attr(&e, "mimeType").unwrap_or_else(|| set_kind.clone()),
                        width: attr(&e, "width").and_then(|v| v.parse().ok()).unwrap_or(0),
                        height: attr(&e, "height").and_then(|v| v.parse().ok()).unwrap_or(0),
                        fps: attr(&e, "frameRate").map(|v| frame_rate(&v)).unwrap_or(0),
                        bandwidth: attr(&e, "bandwidth").and_then(|v| v.parse().ok()).unwrap_or(0),
                        codecs: attr(&e, "codecs").unwrap_or_default(),
                    });
                }
                b"BaseURL" => base = Some(String::new()),
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let (Some(b), Ok(text)) = (base.as_mut(), t.decode()) {
                    b.push_str(&text);
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let (Some(b), Ok(name)) = (base.as_mut(), r.decode()) {
                    match name.as_ref() {
                        "amp" => b.push('&'),
                        "lt" => b.push('<'),
                        "gt" => b.push('>'),
                        "quot" => b.push('"'),
                        "apos" => b.push('\''),
                        other => {
                            let code = other
                                .strip_prefix("#x")
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .or_else(|| other.strip_prefix('#').and_then(|d| d.parse().ok()));
                            if let Some(c) = code.and_then(char::from_u32) {
                                b.push(c);
                            }
                        }
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"BaseURL" => {
                    if let (Some(url), Some(r)) = (base.take(), rep.as_ref()) {
                        let kind = format!("{} {set_kind}", r.mime);
                        let url = url.trim().to_string();
                        if kind.contains("video") {
                            videos.push(VideoFormat { url, width: r.width, height: r.height, fps: r.fps, codec_rank: select::codec_rank(&r.codecs), bitrate: r.bandwidth });
                        } else if kind.contains("audio") {
                            audios.push(AudioFormat { url, is_default_track: true, is_drc: false, opus: r.codecs.contains("opus"), bitrate: r.bandwidth });
                        }
                    }
                }
                b"Representation" => rep = None,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    (videos, audios)
}

pub async fn resolve(http: &reqwest::Client, url: &url::Url, max_res: u32, max_fps: u32) -> Result<Content, Fail> {
    let (kind, code) = shortcode(url.path()).ok_or_else(|| Fail::Definitive("not a post URL".into()))?;
    let page_url = format!("https://www.instagram.com/{kind}/{code}/");
    let resp = http
        .get(&page_url)
        .header("User-Agent", DESKTOP_UA)
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Dest", "document")
        .send()
        .await?;
    if resp.status().is_redirection() {
        return Err(Fail::Transient(format!("redirected ({}), likely rate-limited", resp.status())));
    }
    if !resp.status().is_success() {
        return Err(Fail::Transient(format!("page HTTP {}", resp.status())));
    }
    let code_needle = format!("\"code\":\"{code}\"");
    let mut body: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    let mut anchor: Option<usize> = None;
    while let Some(chunk) = stream.next().await {
        let before = body.len();
        body.extend_from_slice(&chunk?);
        if anchor.is_none() {
            anchor = find_bytes(&body, code_needle.as_bytes(), before.saturating_sub(code_needle.len()));
        }
        if let Some(a) = anchor {
            if let Some(manifest) = json_string_after(&body, "video_dash_manifest", a.saturating_sub(64 * 1024)) {
                let (videos, audios) = parse_manifest(&manifest);
                if let Some(v) = select::pick_video(&videos, max_res, max_fps) {
                    return Ok(Content {
                        video: v.url.clone(),
                        audio: select::pick_audio(&audios).map(|x| x.url.clone()),
                        title: None,
                        width: Some(v.width),
                        height: Some(v.height),
                        headers: vec![("Referer".into(), "https://www.instagram.com/".into())],
                    });
                }
            }
            let lo = a.saturating_sub(8 * 1024);
            let hi = (a + 8 * 1024).min(body.len());
            let near = &body[lo..hi];
            if find_bytes(near, b"\"media_type\":1,", 0).is_some() || find_bytes(near, b"\"media_type\":1}", 0).is_some() {
                return Err(Fail::NotVideo("photo post".into()));
            }
        }
        if body.len() > MAX_PAGE_BYTES {
            break;
        }
    }
    Err(Fail::Definitive("page without video data (gated, private or photo post)".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shortcodes() {
        assert_eq!(shortcode("/reel/Chunk8-jurw/"), Some(("reel".into(), "Chunk8-jurw".into())));
        assert_eq!(shortcode("/user/p/ABC/"), Some(("p".into(), "ABC".into())));
        assert_eq!(shortcode("/user/"), None);
    }

    #[test]
    fn parses_manifest() {
        let xml = r#"<MPD><Period><AdaptationSet contentType="video"><Representation width="720" height="1280" frameRate="30" bandwidth="3000000" codecs="avc1.64001F"><BaseURL>https://cdn/v.mp4?a=1&amp;b=2</BaseURL></Representation><Representation width="472" height="840" bandwidth="100000" codecs="avc1"><BaseURL>https://cdn/s.mp4</BaseURL></Representation></AdaptationSet><AdaptationSet contentType="audio"><Representation bandwidth="96000" codecs="mp4a.40.5"><BaseURL>https://cdn/a.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>"#;
        let (v, a) = parse_manifest(xml);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].url, "https://cdn/v.mp4?a=1&b=2");
        assert_eq!(a[0].url, "https://cdn/a.mp4");
    }

    #[test]
    fn extracts_json_string() {
        let page = br#"..."code":"X","video_dash_manifest":"<MPD a=\"1\"></MPD>"..."#;
        let s = json_string_after(page, "video_dash_manifest", 0).unwrap();
        assert_eq!(s, r#"<MPD a="1"></MPD>"#);
    }
}
