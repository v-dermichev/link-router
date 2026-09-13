# Plugins

Status: design.

## What a plugin is

A plugin is a Lua module that adds support for a kind of content or a set of
sites by calling the same `router` API a config uses: it registers routes,
resolvers, targets and event handlers. There is no separate plugin language,
manifest format or build step (see [decisions.md](decisions.md#d4-lua-is-both-the-config-and-the-plugin-language)).

Anything a plugin does can also be written inline in `init.lua`. A plugin is
the same code packaged for reuse, with options and a namespace.

## Layout

```
~/.config/link-router/
├── init.lua
└── plugins/
    └── image/
        ├── init.lua          -- returns the plugin spec
        ├── hosts.lua         -- require("image.hosts")
        └── README.md
```

A single file `plugins/<name>.lua` works too. First-party plugins have the
same layout, embedded in the binary.

## Declaring a plugin

`plugins/<name>/init.lua` returns a spec built with `router.plugin`:

```lua
return router.plugin({
  name = "image",
  api = 1,                           -- core plugin API version this was written for (versioning pending Q5)
  description = "Direct image links and image hosts in an image viewer",
  schemes = {},                      -- extra URL schemes to intercept (added to interception.schemes)

  options = {                        -- defaults; the user's options are merged over them
    viewer = { "imv", "{path}" },
    hosts = { "i.imgur.com", "pbs.twimg.com" },
  },

  setup = function(opts, ctx)
    router.target({
      name = "viewer",               -- registered as "image.viewer"
      exec = opts.viewer,
      accepts = { "image" },
    })

    router.route({
      name = "direct",               -- "image.direct"
      match = { host = opts.hosts },
      resolve = { "core.download" },
      target = "viewer",
    })

    router.route({
      name = "by-extension",
      match = { path = { "(?i)\\.(?:png|jpe?g|gif|webp|avif)$" } },
      resolve = { "core.download" },
      target = "viewer",
    })
  end,
})
```

imv opens local paths only, so the routes download first and the target uses
`{path}`.

## Loading

```lua
router.use("mpv-video", { quality = { max_resolution = 720 } })
router.use("image")
```

- **Lookup order:** `~/.config/link-router/plugins/<name>` first, then the
  first-party plugin embedded in the binary. Copying a shipped plugin into the
  config directory replaces it.
- **Route order:** a plugin's routes are inserted at the position of its
  `router.use` call. Routes declared before it in `init.lua` match first,
  routes after it match later, and the implicit fallback route is always last.
- **`require`:** during `setup`, the plugin's directory is on `package.path`
  under its name, so `require("image.hosts")` loads `plugins/image/hosts.lua`.
- **API version:** `api` must be a version the core supports, or the plugin is
  not loaded and the error is reported.
- **Schemes:** a plugin's `schemes` are added to the core's
  `interception.schemes`; the sentinel shadows each scheme's default handler
  ([integration.md](integration.md#intercepted-schemes)).

### Options

The user's options are merged over the plugin's `options` defaults:

- Tables with string keys merge recursively.
- Lists (tables with integer keys) are replaced as a whole, so
  `viewer = { "feh", "{path}" }` replaces the default viewer argv entirely.
- A key that doesn't exist in the defaults is an error, unless the default
  for its parent is an empty table (`{}`), which marks a free-form table.

## Names

- Everything a plugin registers is named `<plugin>.<name>`.
- Config-level names contain no dots; dotted names are reserved for plugins
  and the core (`core.download`).
- Inside a plugin, an undotted name refers to the plugin's own registration:
  `target = "viewer"` means `image.viewer`. This applies at load time and at
  link time: the plugin namespace is captured in every function created during
  `setup`, so `ctx.target("viewer")` and `router.act.resolve({ "direct" }, …)`
  in a plugin function resolve the same way, and so does
  `router.on("viewer:content_shown", …)`.
- A dotted name is always global: `"mpv-video.ytdlp"` from another plugin or from
  the config.
- A plugin referring to a config-level name writes it with a leading `@`:
  `"@invidious"`.

## Overriding plugin behaviour from the config

Registrations are named objects with `_update` and `_remove` functions
([lua-api.md](lua-api.md#updating-and-removing)), so the config can change a
plugin's behaviour after `router.use`:

```lua
router.use("mpv-video")

-- A config-level resolver in front of the plugin's chain.
router.resolver({
  name = "invidious",
  family = "invidious",
  run = function(link, ctx)
    -- ask a self-hosted Invidious instance for stream URLs
  end,
})

router.route_update("mpv-video.youtube", {
  resolve = { "invidious", "mpv-video.youtube_direct", "mpv-video.ytdlp" },
})

-- Remove a route the plugin added.
router.route_remove("mpv-video.instagram")

-- Add a route that reuses the plugin's resolver and target.
router.route({
  name = "vimeo",
  match = { host = { "vimeo.com", "*.vimeo.com" } },
  resolve = { "mpv-video.ytdlp" },
  target = "mpv-video.player",
})
```

## Errors

| Error | Effect |
|---|---|
| Syntax or runtime error in `init.lua` itself | The whole load fails; the previous config stays active; notification |
| A plugin can't be found or has an unsupported `api` | That plugin is skipped; routes and updates in the config that refer to its names are dropped; everything else loads; notification |
| Error inside a plugin's `setup` | That plugin's registrations are discarded, with the same handling of references to them; everything else loads; notification |
| Unknown option key | The plugin is skipped as above |
| Error in a plugin function at link time (resolver, handler, event) | Logged in the link's trace; handled as a failure of that step (next resolver, or the fallback) |

## State

All plugins share one Lua state per config load; a reload builds a new state
and runs every `setup` again. Links already in progress finish on the Lua state
they started on. State that must survive reloads or daemon restarts goes
through `ctx.cache` with a `ttl`, namespaced per plugin.

## Performance rules for plugins

The core's promise is that a plugin's handling costs little beyond the work
it asks for. That holds if plugins follow these rules:

- **Prefer declarative `match` tables.** They're matched in Rust without
  entering Lua. A `fn` predicate runs Lua for every link that reaches it, and
  a `link.received` callback runs Lua for every link.
- **Use services for heavy lifting.** HTTP reads, streaming reads that stop
  early, JSON/XML/HTML parsing and app control are Rust services; the Lua side
  should mostly pass data between them.
- **Start work early.** For `resolve` + `target` routes the core calls the
  target's `prepare()` when the chain starts, so a player starts in parallel
  with the resolver's request. Resolvers can also call `ctx.net.preconnect` for
  hosts they will need (the `mpv-video` plugin opens googlevideo connections before
  the player API request returns).

Whether the `mpv-video` plugin's per-link Lua logic stays below measurable cost is
**pending** Q4 in [decisions.md](decisions.md#open-questions). If it doesn't, the
answer is a new core service (for example `json.select`), not native plugin
code.

## First-party plugins

| Plugin | Status | Scope |
|---|---|---|
| `mpv-video` | designed ([plugins/mpv-video.md](plugins/mpv-video.md)) | YouTube and Instagram direct resolvers, yt-dlp fallback for other sites, one managed mpv window |
| `image` | idea | Direct image URLs and image hosts in an image viewer, galleries as a content list |
| `audio` | idea | Audio files and audio sites (via yt-dlp) in an audio-only player instance, with append-to-queue |
| `document` | idea | PDFs and similar: `core.download`, then a document viewer's desktop entry |

Only `mpv-video` is being designed now. The others are listed to keep the core API
honest: each of them should be writable with the API as designed, and a gap
found while sketching one is a gap in the core.

## Out of scope

- A plugin registry or package manager. Plugins are directories; git clones
  work.
- Sandboxing. Plugins run with the same privileges as the config.
- Plugins in other languages. A Lua plugin can call any program through
  `ctx.exec` when that is really needed.
