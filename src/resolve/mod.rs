//! Resolvers of the mpv-video plugin: a link becomes stream URLs.

pub mod instagram;
pub mod select;
pub mod youtube;
pub mod ytdlp;

use std::fmt;

#[derive(Debug, Clone, Default)]
pub struct Content {
    pub video: String,
    pub audio: Option<String>,
    pub title: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum Fail {
    /// The service rejected the link; other resolvers won't do better.
    Definitive(String),
    /// Network trouble, an unexpected response or content this resolver doesn't
    /// handle (live streams); the next resolver may succeed.
    Transient(String),
    /// Not a video at all (e.g. a photo post): go straight to the browser.
    NotVideo(String),
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fail::Definitive(r) => write!(f, "definitive: {r}"),
            Fail::Transient(r) => write!(f, "transient: {r}"),
            Fail::NotVideo(r) => write!(f, "not a video: {r}"),
        }
    }
}

impl From<reqwest::Error> for Fail {
    fn from(e: reqwest::Error) -> Self {
        Fail::Transient(e.to_string())
    }
}

/// Whether the URL path ends in one of `extensions`, e.g. `https://h/a/b.webm?x=1`.
pub fn is_direct_media(url: &url::Url, extensions: &[String]) -> bool {
    let name = url.path().rsplit('/').next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    YouTube,
    Instagram,
}

pub fn site_of(url: &url::Url) -> Option<Site> {
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path();
    if host == "youtu.be" {
        return (path.len() > 1).then_some(Site::YouTube);
    }
    if host == "youtube.com" || host.ends_with(".youtube.com") {
        let video_path = path == "/watch" && url.query_pairs().any(|(k, _)| k == "v");
        let prefixed = ["/shorts/", "/live/", "/embed/"].iter().any(|p| path.starts_with(p));
        return (video_path || prefixed).then_some(Site::YouTube);
    }
    if host == "instagram.com" || host == "www.instagram.com" {
        return instagram::shortcode(path).map(|_| Site::Instagram);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(u: &str) -> Option<Site> {
        site_of(&url::Url::parse(u).unwrap())
    }

    #[test]
    fn matches_sites() {
        assert_eq!(site("https://www.youtube.com/watch?v=jNQXAC9IVRw"), Some(Site::YouTube));
        assert_eq!(site("https://youtube.com/shorts/j2Ky4AJ1OvA?si=x"), Some(Site::YouTube));
        assert_eq!(site("https://youtu.be/jNQXAC9IVRw"), Some(Site::YouTube));
        assert_eq!(site("https://m.youtube.com/watch?v=abc"), Some(Site::YouTube));
        assert_eq!(site("https://www.youtube.com/@chan"), None);
        assert_eq!(site("https://www.youtube.com/results?search_query=a"), None);
        assert_eq!(site("https://example.com/youtube.com/watch?v=x"), None);
        assert_eq!(site("https://www.instagram.com/reel/Chunk8-jurw/"), Some(Site::Instagram));
        assert_eq!(site("https://www.instagram.com/someone/reel/Chunk8-jurw/"), Some(Site::Instagram));
        assert_eq!(site("https://www.instagram.com/someone/"), None);
    }

    #[test]
    fn matches_direct_media() {
        let exts: Vec<String> = crate::config::DIRECT_EXTENSIONS.iter().map(|e| e.to_string()).collect();
        let direct = |u: &str| is_direct_media(&url::Url::parse(u).unwrap(), &exts);
        assert!(direct("https://img-9gag-fun.9cache.com/photo/a9y3790_460svvp9.webm"));
        assert!(direct("https://cdn.example/v/clip.MP4?token=a.b"));
        assert!(!direct("https://example.com/watch.mp4/"));
        assert!(!direct("https://example.com/.webm"));
        assert!(!direct("https://example.com/src/index.ts"));
        assert!(!direct("https://example.com/page?file=a.mp4"));
    }
}
