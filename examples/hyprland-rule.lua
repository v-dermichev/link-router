-- Window rule for the `mpv-video` plugin's player (app id from the plugin's
-- player.app_id). The plugin re-fits the window to each video's aspect ratio
-- through the compositor adapter; this is the initial landscape shape,
-- bottom-right 960x540 of a 1920x1080 gapped area (x 35-1885, y 55-1055).
hl.window_rule({
  name  = "mpv-video",
  match = { class = "^(mpv-video)$" },
  float = true,
  size  = "960 540",
  move  = "925 515",
})
