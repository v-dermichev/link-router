-- link-router config: the `mpv-video` plugin set up like open-url (YouTube and
-- Instagram videos in one floating mpv in the bottom-right corner, everything
-- else in the default browser, Brave on the test machine), plus three inline
-- routes showing custom handling without a plugin.
-- Copy to ~/.config/link-router/init.lua and run `link-router enable`.

router.config({
  -- No `browser`: links fall back to the default browser they came through.
  -- A resident daemon lets the sentinel notice a default-browser change at once.
  daemon = { idle_exit = false },
  interception = { repair = "auto" },
})

-- Declared before the plugin, so it matches first: a video opened from a
-- playlist (watch?v=…&list=…) stays in the browser instead of reaching
-- mpv-video.youtube.
router.route({
  name = "youtube-playlists",
  match = {
    host = { "youtube.com", "*.youtube.com" },
    path = { "^/watch$" },
    query = { list = ".+" },
  },
  open = "browser",
})

router.use("mpv-video", {
  player = {
    app_id = "mpv-video",
    -- Hybrid laptop with both displays on the Intel GPU: render with OpenGL on
    -- Intel (Vulkan only sees NVIDIA and takes ~0.7 s to start), decode with
    -- VA-API through Intel's driver, keep NVIDIA's GL libraries out, and don't
    -- send the content-type hint that makes the HDMI monitor re-sync in fullscreen.
    args = { "--vo=gpu-next", "--gpu-api=opengl", "--hwdec=vaapi", "--wayland-content-type=none" },
    env = {
      LIBVA_DRIVER_NAME = "iHD",
      __EGL_VENDOR_LIBRARY_FILENAMES = "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
      __GLX_VENDOR_LIBRARY_NAME = false,
    },
  },
  quality = {
    max_resolution = 720,
    max_fps = 60,
    buffer = "2s",
  },
  window = {
    box = { 960, 720 },
    anchor = "bottom-right",
    margin = { 35, 25 },
    follow_focus = true,
  },
})

-- Reuse the plugin's resolver and player for a site it doesn't cover.
router.route({
  name = "vimeo",
  match = { host = { "vimeo.com", "*.vimeo.com" } },
  resolve = { "mpv-video.ytdlp" },
  target = "mpv-video.player",
})

-- Direct image links in imv. imv opens local files only, so download first.
router.target({ name = "imv", exec = { "imv", "{path}" }, accepts = { "image" } })

router.route({
  name = "images",
  match = {
    scheme = { "https", "http" },
    path = { "(?i)\\.(?:png|jpe?g|gif|webp|avif)$" },
  },
  resolve = { "core.download" },
  target = "imv",
})

-- Know when a fast resolver stops working, instead of just getting slower.
router.on("resolve.failed", function(e, ctx)
  if e.failure.kind == "definitive" and e.failure.resolver:match("_direct$") then
    ctx.log.warn(("%s failed for %s: %s"):format(e.failure.resolver, e.link.url, e.failure.reason))
  end
end)
