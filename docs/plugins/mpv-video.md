# Plugin: `mpv-video`

Status: design. Request shapes and timings come from
open-url's design and its reviews (the unpublished prototype);
numbers are marked **MEASURED** or **ESTIMATED** there and here.

First-party plugin, embedded in the binary. YouTube and Instagram videos (and
anything yt-dlp supports, when a route sends it there) play in one managed mpv
window, placed and sized through the compositor adapter.

## Options

```lua
router.use("mpv-video", {
  player = {
    app_id = "mpv-video",               -- Wayland app id, what compositor rules match
    args = {},                          -- extra mpv arguments for a new player (see Player settings)
    env = {},                           -- extra environment for the player process, e.g. { LIBVA_DRIVER_NAME = "iHD" }
    network_timeout = "8s",             -- per-file mpv network timeout for stream URLs
    stream_options = { tcp_nodelay = "1" }, -- --stream-lavf-o values for items loaded without the proxy
    ca_file = nil,                      -- --tls-ca-file for items loaded without the proxy (see Playback path)
  },
  quality = {
    max_resolution = 1080,              -- shorter side, so a 720x1280 reel counts as 720
    max_fps = 60,
    buffer = "1s",                      -- 0 starts without an initial buffer
  },
  window = {
    box = { 960, 720 },                 -- fit each video's aspect ratio inside this box
    anchor = "bottom-right",
    margin = { 35, 25 },                -- from the anchored corner, logical pixels
    follow_focus = true,                -- move the player to the workspace focused when the link arrived
  },
  sites = { youtube = true, instagram = true },
})
```

## What it registers

| Kind | Name | Purpose |
|---|---|---|
| route | `mpv-video.youtube` | match below → chain `mpv-video.youtube_direct`, `mpv-video.youtube_ytdlp_fast`, `mpv-video.ytdlp` → `mpv-video.player` |
| route | `mpv-video.instagram` | match below → chain `mpv-video.instagram_direct`, `mpv-video.ytdlp` → `mpv-video.player` |
| resolver | `mpv-video.youtube_direct` | one `visionos` player API request; family `youtube-visionos` |
| resolver | `mpv-video.youtube_ytdlp_fast` | yt-dlp with `visionos`, skipped page/HLS, cached visitor data; family `youtube-visionos` |
| resolver | `mpv-video.instagram_direct` | reel page read until the media object, DASH representation choice; family `instagram-page` |
| resolver | `mpv-video.ytdlp` | `yt-dlp -J` with default extractor arguments; no family; usable by any route |
| target | `mpv-video.player` | mpv driver, `accepts = { "video", "audio" }` |
| event | `player:video_params` | `ctx.compositor.fit_window({ target = "mpv-video.player" }, …)` with the video's display size (`e.dw`, `e.dh`) and the `window` options; with `follow_focus`, first moves the window to `e.link.workspace` |

Route matches:

```lua
-- mpv-video.youtube
match = {
  host = { "youtube.com", "*.youtube.com", "youtu.be" },
  url = {
    "^https?://youtu\\.be/[A-Za-z0-9_-]{11}",
    "^https?://(?:[^/]+\\.)?youtube\\.com/(?:watch\\?|shorts/|live/|embed/)",
  },
}

-- mpv-video.instagram
match = {
  host = { "instagram.com", "www.instagram.com" },
  path = { "^/(?:[^/]+/)?(?:p|tv|reels?)/[^/]+" },
}
```

Other routes can reuse the resolvers and the target, e.g. a config route for
Vimeo with `resolve = { "mpv-video.ytdlp" }, target = "mpv-video.player"`.

## Content produced

Resolvers return `router.content` with `kind = "video"`, `url` = the video
stream, `extra = { { role = "audio", url = … } }` when audio is a separate
stream, `title`, `http_headers`, and `meta = { live, width, height }`. Width and
height come from the chosen format, so the window can be placed before the
first frame. Instagram carousels return a `router.content_list` whose first
item plays (**pending** Q7).

## Failure families

- `mpv-video.youtube_direct` and `mpv-video.youtube_ytdlp_fast` both ask YouTube's
  `visionos` client and share the family `youtube-visionos`. A rejection from
  YouTube (`playabilityStatus`, wrong video id, live, no formats with `url`) is
  `definitive`, so the chain skips `mpv-video.youtube_ytdlp_fast` and goes straight
  to `mpv-video.ytdlp`. Repeating the request can only fail the same way ("Made
  for kids" videos are unavailable to `visionos`; live streams fail `skip=hls`).
- Transport errors (timeouts, connection failures, malformed responses) are
  `transient`, so `mpv-video.youtube_ytdlp_fast` still gets its turn: yt-dlp may
  handle a YouTube change the direct resolver doesn't.
- A stream that fails in the player before `content_shown` (a googlevideo 403, a
  failed parallel audio open, the network timeout) is a `definitive` failure of
  the resolver that produced it: a 403 on `visionos` stream URLs skips to
  `mpv-video.ytdlp`.

## Resolvers

All of them return direct stream URLs; the player never gets a page URL.

### `mpv-video.youtube_direct`

Request: `POST https://www.youtube.com/youtubei/v1/player?prettyPrint=false` as
the `visionos` client (context from yt-dlp's `INNERTUBE_CLIENTS`), with
`X-Goog-Visitor-Id` and an `X-Goog-FieldMask`:

- `playabilityStatus.status`, `playabilityStatus.reason`
- `videoDetails.videoId`, `videoDetails.title`, `videoDetails.isLive`
- `streamingData.adaptiveFormats.{itag,url,mimeType,width,height,fps,bitrate,isDrc,audioTrack,type,targetDurationSec}`
- `responseContext.visitorData`

`visionos` is the only no-JS client whose HTTPS stream URLs need no PO token;
its URLs are plain signed URLs with no `n` parameter. yt-dlp notes PO-token
enforcement spreading to `android_vr` (**pending** Q12).

When the link arrives, the resolver calls
`ctx.net.preconnect("https://<last edge host>", 2)` so two googlevideo
connections are open by the time the player request returns (handshake
60–90 ms against a request of 136–297 ms warm to cold, **MEASURED**), and
`ctx.net.warm_dns` for the last edge node's `rrN` hosts.

Checks before selecting formats (each `definitive`):

- `playabilityStatus.status == "OK"`
- `videoDetails.videoId` equals the requested id
- not live, not post-live DVR, not upcoming
- at least one video and one audio format with a `url` (a SABR-only response
  omits `url`)

Format selection:

- Drop formats without `url`, OTF formats (`type == "FORMAT_STREAM_TYPE_OTF"`)
  and formats with `targetDurationSec`, as yt-dlp does.
- Video: largest by (shorter side ≤ `max_resolution`, fps ≤ `max_fps`, codec
  rank av01 > vp9 > avc1, bitrate); if nothing fits, the smallest. Software
  first-frame decode is 11–21 ms for all three codecs on the test machine and
  AV1 has the lowest bitrate (**MEASURED**), so the rank stays.
- Audio: prefer the original/default `audioTrack` (not a dub), then
  `isDrc != true`, then Opus, then bitrate. DRC and non-DRC variants share an
  itag with bitrates within 0.03 % (**MEASURED**).

Visitor data:

- Kept with `ctx.cache.set("visitor_data", value, "30d")`.
- Every player response carries a rotated `responseContext.visitorData`; the
  resolver stores it, so normal use refreshes the cache.
- Cold start: `POST /youtubei/v1/visitor_id` (WEB context), 211 ms, and a
  `visionos` request with that value returned `OK` with 16 formats
  (**MEASURED**; yt-dlp doesn't use this endpoint). The homepage scrape is the
  fallback source.

Request timing (**MEASURED**): cold 252–297 ms (curl and rustls prototypes; in
the curl runs TCP+TLS took 0.12–0.16 s and the server 0.11–0.18 s); on a warm
HTTP/2 connection 136–165 ms. The field mask shrinks the compressed response to
3.1–3.9 KB.

### `mpv-video.youtube_ytdlp_fast`

yt-dlp with
`--extractor-args "youtube:player_client=visionos;player_skip=webpage,configs,initial_data;skip=hls;visitor_data=…"`,
using the cached visitor data, run by the resolver through `ctx.exec` (not by
mpv's ytdl hook). Kept for when `mpv-video.youtube_direct` breaks on a YouTube-side
change that yt-dlp has already adapted to.

### `mpv-video.instagram_direct`

Request: `GET https://www.instagram.com/<type>/<shortcode>/` (`<type>` is `reel`,
`reels`, `p` or `tv`) with a desktop browser User-Agent and navigation headers
(`Sec-Fetch-Mode: navigate` etc.), no cookies, redirects not followed, read with
`ctx.http.stream` over the daemon's Instagram connection (kept warm with HTTP/2
PING, see [architecture.md](../architecture.md#connections-and-tls)).

- A redirect to `/accounts/login` means rate-limited (`transient`).
- Shortcodes longer than 28 characters are private posts: `not_applicable`
  without a request (as yt-dlp does).
- The resolver stops reading once the post's media object is complete. It
  anchors on `xig_polaris_media` with a matching `code`, not on the first
  `"video_versions"` in the page. Stopping early saves ~60 ms; the media
  arrives after a server-side pause (**MEASURED**).
- The stream ending without media is `definitive` (gated, private, photo post
  or markup change): the chain continues with `mpv-video.ytdlp`, which usually
  fails the same way, and then the browser. One of five fetches in testing
  came back without media for no identified reason (**pending** Q11).

Stream choice (**MEASURED** on one reel):

- All `video_versions` entries were the same 480×854 progressive file.
- The `video_dash_manifest` in the same response has 720×1280 representations
  (3.14, 1.97, 1.04, 0.51, 0.24 Mbps) and 472×840, each a single fMP4 file
  (`SegmentBase`). The resolver parses it with `ctx.xml`, picks the video
  representation by shorter side ≤ `max_resolution` then bandwidth, and adds
  the audio adaptation set (if any) as the `audio` extra. mpv opened the top
  representation with no seeks.
- Carousel posts (`/p/` with `carousel_media`): a content list; **pending** Q7.

HTTP/3 saved about one round trip on this request (connect+TLS 55 ms vs 134 ms,
**MEASURED**, n=1).

### `mpv-video.ytdlp`

`yt-dlp -J --no-playlist -f "bv*+ba/b" -S "res:<max>,fps:<max>" URL` through
`ctx.exec`; the resolver maps the chosen `requested_formats` to the content's
`url` and `audio` extra, with `http_headers` from the JSON, so yt-dlp results
take the same parallel playback path as the direct resolvers. This handles
everything without a built-in resolver. Python startup (~0.25 s) stays on this
path; a warm yt-dlp worker is **pending** Q8.

## Player settings

The mpv driver starts the player with `--ytdl=no` (resolution never happens in
mpv, and a loaded ytdl hook would run yt-dlp on any stream URL that fails to
open, delaying the failure by ~0.8 s, **MEASURED**) plus the plugin's `args` and
`env`.

Recommended for hybrid-GPU laptops whose displays run on an Intel GPU while the
session points VA-API/GLX at NVIDIA (the test machine):

```lua
player = {
  args = { "--vo=gpu-next", "--gpu-api=opengl", "--hwdec=vaapi", "--wayland-content-type=none" },
  env = {
    LIBVA_DRIVER_NAME = "iHD",
    __EGL_VENDOR_LIBRARY_FILENAMES = "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
    __GLX_VENDOR_LIBRARY_NAME = false,   -- false unsets
  },
},
```

| Setting | Why (MEASURED on the test machine) |
|---|---|
| `--gpu-api=opengl` | Renders on the Intel GPU; exec to first IPC reply 126–149 ms vs 697–784 ms with Vulkan, which only sees the NVIDIA GPU |
| `--vo=gpu-next` | No output fallback chain, so a GL failure can't end on an output that renders on NVIDIA |
| `--hwdec=vaapi` + `LIBVA_DRIVER_NAME=iHD` | Same first-frame time as software decoding, about two thirds less playback CPU (0.78–0.85 s vs 2.15–2.34 s of CPU over 10 s) |
| Mesa-only EGL, GLX vendor unset | NVIDIA's EGL/GLX libraries aren't loaded; a few tens of ms less startup |
| `--wayland-content-type=none` | The default "video" content-type hint makes Hyprland pass the content type to the HDMI monitor in fullscreen, which blanks it for seconds while it re-syncs; no other application triggered this |

These are recommendations, not defaults: other machines may have no Intel
VA-API driver or different paths. The plugin's defaults are plain mpv.

A pre-started idle player is not offered: it gained 0–10 ms, and an idle player
without a window was 20–90 ms slower (**MEASURED**).

## Playback path

With the core's streaming proxy (default when available, see
[architecture.md](../architecture.md#streaming-proxy)): the driver registers the
video and audio URLs with the proxy, which starts both fetches on the warm
connections, and loads an EDL of the two localhost URLs. First frame 66 ms after
`loadfile` with connections already open, 151 ms with new ones (**MEASURED**,
vo=null).

Without the proxy (disabled, or the proxy failed for this item): the driver
loads the video URL and adds the audio in parallel from the bundled mpv script,
which defers playback until the audio track is added so its start isn't
clipped ([integration.md](../integration.md#opening-content)); first frame
0.21–0.25 s after `loadfile` (**MEASURED**). In that mode the player uses the
`stream_options` and `ca_file` options: `tcp_nodelay=1` is the default, and
together with a one-certificate `ca_file` (which only works while mpv's
`tls-verify` is off, mpv's default) it saved ~57 ms per stream open
(**MEASURED**). `ca_file` has no default because the right certificate depends
on the hosts; it is a documented recommendation.

## Timing budget

YouTube Short, running player, stages summed (**ESTIMATED** from measured
stages; vo=null for the player part, display cost excluded):

| Stage | Warm (within 240 s of the last YouTube request) | Idle past 240 s, 0-RTT¹ | Cold |
|---|---|---|---|
| Client exec to socket write | 0.3 ms | 0.3 ms | 0.3 ms |
| Daemon IPC, routing, `link.received` absent | ~1 ms | ~1 ms | ~1 ms |
| Player API request | 136–165 ms | ~200–212 ms | 252–297 ms |
| Format selection (Lua over JSON service) | < 1 ms, **pending** Q4 | < 1 ms | < 1 ms |
| Registering streams and `loadfile` to first frame through the proxy (EDL) | 66–74 ms | 66–74 ms | ~159 ms |
| **Total** | **≈ 0.20–0.24 s** | **≈ 0.27–0.29 s** | **≈ 0.41–0.46 s** |

¹ Requires persisting TLS sessions across daemon restarts, which needs an
OpenSSL build (**pending** Q1); the 200–212 ms come from a two-sample test whose
own cold baseline was 270 ms.

Add initial buffering (`quality.buffer`, zero on a fast link) and the window's
first paint. A new player starts in parallel with the request, so its total is
roughly the larger of the request path and player startup (~0.13–0.15 s with
OpenGL).

Instagram on a warm connection: page to media ~0.45–0.65 s plus stream open
~0.15 s ≈ **0.6–0.8 s** (**ESTIMATED**; the Instagram CDN's stream open was not
measured through the proxy, and one direct request took 0.91 s to first byte,
**pending** Q14).

For comparison, the bash implementation in open-url measures 1.18–1.29 s
with `FAST_YOUTUBE` (**MEASURED**), and its curl-based `direct` design is
estimated at ~0.5 s (**ESTIMATED**).
