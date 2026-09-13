//! Format choice with yt-dlp's `-S res:N,fps:N` semantics: prefer the largest
//! value within the limit, else the closest above; resolution before frame rate.

#[derive(Debug, Clone)]
pub struct VideoFormat {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec_rank: u8,
    pub bitrate: u64,
}

#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub url: String,
    pub is_default_track: bool,
    pub is_drc: bool,
    pub opus: bool,
    pub bitrate: u64,
}

/// Higher is better: within the limit the largest value wins, above it the smallest.
fn limit_key(value: u32, limit: u32) -> (u8, i64) {
    if value <= limit {
        (1, value as i64)
    } else {
        (0, -(value as i64))
    }
}

pub fn codec_rank(mime_or_codec: &str) -> u8 {
    let c = mime_or_codec.to_ascii_lowercase();
    if c.contains("av01") || c.contains("av1") {
        3
    } else if c.contains("vp9") || c.contains("vp09") {
        2
    } else if c.contains("avc") || c.contains("h264") {
        1
    } else {
        0
    }
}

pub fn pick_video(formats: &[VideoFormat], max_res: u32, max_fps: u32) -> Option<&VideoFormat> {
    formats.iter().max_by_key(|f| {
        let short = f.width.min(f.height);
        (limit_key(short, max_res), limit_key(f.fps, max_fps), f.codec_rank, f.bitrate)
    })
}

pub fn pick_audio(formats: &[AudioFormat]) -> Option<&AudioFormat> {
    formats.iter().max_by_key(|a| (a.is_default_track, !a.is_drc, a.opus, a.bitrate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(w: u32, h: u32, fps: u32, codec: u8, br: u64) -> VideoFormat {
        VideoFormat { url: format!("{w}x{h}@{fps}c{codec}"), width: w, height: h, fps, codec_rank: codec, bitrate: br }
    }

    #[test]
    fn prefers_within_limits_then_codec() {
        let f = vec![v(1920, 1080, 30, 3, 1), v(1280, 720, 30, 1, 9), v(1280, 720, 30, 3, 2), v(854, 480, 30, 3, 1)];
        assert_eq!(pick_video(&f, 720, 60).unwrap().url, "1280x720@30c3");
    }

    #[test]
    fn portrait_uses_short_side() {
        let f = vec![v(720, 1280, 30, 3, 1), v(1080, 1920, 30, 3, 1)];
        assert_eq!(pick_video(&f, 720, 60).unwrap().url, "720x1280@30c3");
    }

    #[test]
    fn keeps_resolution_when_only_fps_misses() {
        let f = vec![v(1920, 1080, 60, 3, 1), v(640, 360, 60, 3, 1)];
        assert_eq!(pick_video(&f, 1080, 30).unwrap().url, "1920x1080@60c3");
    }

    #[test]
    fn closest_above_when_nothing_fits() {
        let f = vec![v(1920, 1080, 30, 1, 1), v(1280, 720, 30, 1, 1)];
        assert_eq!(pick_video(&f, 480, 30).unwrap().url, "1280x720@30c1");
    }

    #[test]
    fn audio_prefers_default_non_drc_opus() {
        let a = |u: &str, def, drc, opus, br| AudioFormat { url: u.into(), is_default_track: def, is_drc: drc, opus, bitrate: br };
        let f = vec![a("drc", true, true, true, 200), a("dub", false, false, true, 300), a("ok", true, false, true, 130), a("aac", true, false, false, 130)];
        assert_eq!(pick_audio(&f).unwrap().url, "ok");
    }
}
