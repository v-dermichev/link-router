# Lua API

Status: design. Names and shapes are proposals; the generated LuaLS stubs
(`link-router lua-stubs`) will be the reference once implemented.

## Model

The config is a Lua script, `~/.config/link-router/init.lua`, executed once per
daemon start and on every reload, in the same spirit as Hyprland's
`hyprland.lua`: the script calls a global `router` table to declare settings
and rules, and registers functions that run later, when links arrive or apps
report events. Plugins ([plugins.md](plugins.md)) use exactly the same API.

- **Declarations** (`router.config`, `router.route`, `router.resolver`,
  `router.target` and their `_update`/`_remove` forms) are collected while the
  script runs and compiled into Rust structures when it finishes. Routes that
  only use declarative `match` tables are matched entirely in Rust.
- **Functions** (route predicates, handlers, resolvers, event callbacks) run on
  the daemon's Lua thread when needed. They get a `ctx` with the core's
  services ([Services](#services)); service calls look blocking from Lua but
  yield the thread to other links while they wait.
- **`router` vs `ctx`.** `router` holds declarations and value constructors
  only (`router.content`, `router.fail.*`, `router.act.*`). Services exist only
  on `ctx`, so `router.target(spec)` always declares a target and
  `ctx.target(name)` always returns a handle.
- **Order:** routes match in declaration order, first match wins. Routes
  registered by a plugin sit at the position of its `router.use` call. An
  implicit last route passes everything else to the link's fallback: the
  original of the entry the link came through
  ([integration.md](integration.md#opening-through-a-shadow)).
- **Names:** config-level names contain no dots. Plugin registrations are
  named `<plugin>.<name>`; see [plugins.md](plugins.md#names).
- **Load-time checks:** when the script finishes, every name a route, chain or
  update refers to must exist. References to something a skipped plugin would
  have registered drop that route or update, with a notification; everything
  else loads.

Lua runtime: Lua 5.4, vendored and statically linked. Standard libraries are
available; the config and plugins are trusted code.

Durations are strings with a unit (`ms`, `s`, `m`, `h`, `d`: `"300ms"`, `"3s"`,
`"10m"`, `"30d"`) or numbers of seconds.

**Per-link cost.** A link reaches Lua only when a route it is matched against
has a `fn` predicate, its route uses a `handler` or a Lua resolver, or a
`link.received` callback is registered. `link.received` runs synchronously
before routing (it can return an action), so registering it puts Lua on every
link. Events whose return value is ignored are dispatched after the link's
action has started and never delay it.

## Core settings: `router.config(settings)`

Only settings that belong to the core. Content-specific settings (player,
quality, window placement) are plugin options.

```lua
router.config({
  browser = nil,                        -- optional desktop entry id for http(s) fallbacks; default: the entry the link came through

  daemon = {
    idle_exit = "30m",                  -- false: stay resident
  },

  log = { level = "info" },             -- "error" | "warn" | "info" | "debug" | "trace"

  interception = {
    schemes = { "http", "https" },      -- intercepted, together with plugins' schemes
    repair = "ask",                     -- "auto" | "ask" | "report": what the sentinel does when interception breaks
  },

  http = {
    timeout = "5s",                     -- default per request; resolvers and requests can override
    user_agent = nil,                   -- default UA for ctx.http when a request sets none
  },
})
```

- Can be called several times; later calls override earlier keys.
- Unknown keys are an error, so typos fail the load.
- `browser`: by default, a link falls back to the original of the shadowed
  entry it came through, which for `http`/`https` is the XDG default browser
  ([integration.md](integration.md#opening-through-a-shadow)). Setting
  `browser` sends `http`/`https` fallbacks to that desktop entry instead; links
  of other schemes always fall back to their own original. A link started
  without a shadow (`link-router open URL` from a terminal) falls back to the
  current default handler of its scheme. The client's own passthrough branches
  run before the config is loaded, so they ignore `browser`.
- `interception.schemes`: for each scheme (plus plugins' `schemes`), the
  sentinel shadows the default handler's entry. A scheme with no default
  handler is not intercepted; link-router never registers an entry of its own
  ([integration.md](integration.md#intercepted-schemes)).
- `interception.repair`: see [integration.md](integration.md#sentinel).

## Plugins

| Function | Purpose |
|---|---|
| `router.use(name, options?)` | Load a plugin (config directory first, then first-party), merge `options` over its defaults, run its `setup` |
| `router.plugin(spec)` | Used in a plugin's `init.lua` to declare it: `name`, `api`, `description`, `options`, `schemes`, `setup(opts, ctx)` |

Details in [plugins.md](plugins.md).

## Routes

### `router.route(spec)`

```lua
router.route({
  name = "pdf",
  match = {
    scheme = { "https" },
    path = { "(?i)\\.pdf$" },
  },
  resolve = { "core.download" },
  target = "zathura",
})
```

`match` fields (all optional, all must hold; within a field, any entry may
match):

| Field | Type | Semantics |
|---|---|---|
| `scheme` | string list | exact, lowercase; only schemes in the intercepted set (`interception.schemes` and plugins' `schemes`) ever reach routing |
| `host` | glob list | `*.example.com` matches subdomains; `example.com` matches only itself |
| `path` | regex list | Rust `regex` syntax, against the path only; case-sensitive unless the pattern starts with `(?i)` |
| `query` | table | `{ v = ".+" }`: each key must exist and match its regex |
| `url` | regex list | against the whole URL |
| `opener` | string list | executable that opened the link (`"kitty"`, `"telegram-desktop"`); may be empty, detection is **pending** Q10 |
| `content_type` | glob list | `"image/*"`: requires a probe before matching; see [Probing](#probing) |
| `fn` | function | `function(link) return boolean end`, runs after the declarative fields matched; no `ctx` |

Exactly one of these says what to do:

| Field | Meaning |
|---|---|
| `resolve` + `target` | Try resolvers in order; open the first content returned with the target |
| `target` alone | Open the link itself with the target (content `{ url = link.url, source = link, kind = nil }`) |
| `open` | `"browser"` (the link's fallback: `browser` from the config for http(s), else the original of the entry the link came through), or `{ desktop = "firefox" }` |
| `exec` | argv array with placeholders (see [Targets](#targets-routertargetspec)); no shell |
| `handler` | `function(link, ctx) return action end`; returning `nil` goes to the route's fallback |

`target` can be a name or a table keyed by content kind, for chains whose
resolvers produce different kinds:
`target = { video = "mpv-video.player", image = "image.viewer" }`. A kind with no
entry counts as a `definitive` failure of the resolver that produced it.

Other route fields:

| Field | Default | Meaning |
|---|---|---|
| `fallback` | `"browser"` | When the chain is exhausted, the timeout expires or the target fails: `"browser"` (as for `open`), `{ open = … }`, `{ exec = { … } }` or `{ handler = fn }` |
| `timeout` | none | Total budget for the whole chain; when it expires, the fallback runs |
| `prepare` | `true` | For `resolve` + `target` routes with a driver target, the core calls the target's `prepare()` when the chain starts; `false` opts out |
| `open_opts` | `{}` | Per-item options passed to the target for every item this route opens (see [integration.md](integration.md#app-drivers)) |

### Updating and removing

| Function | Semantics |
|---|---|
| `router.route_update(name, fields)` | Replaces the given fields; the route keeps its position. `match` is replaced as a whole. Setting one action field (`resolve`+`target`, `target`, `open`, `exec`, `handler`) clears the others |
| `router.route_remove(name)` | Removes the route |
| `router.resolver_update(name, fields)`, `router.resolver_remove(name)` | Same for resolvers; removing one also removes it from every chain |
| `router.target_update(name, fields)`, `router.target_remove(name)` | Same for targets; a route left without a target fails the load |

Declaring a name that already exists is a load error; use the `_update` forms.

## Resolvers: `router.resolver(spec)`

A resolver turns a link into content, or fails with a reason. Plugins ship
resolvers; the config can add its own.

```lua
router.resolver({
  name = "peertube",
  timeout = "3s",
  family = "peertube-api",            -- string or list; see failure kinds
  run = function(link, ctx)
    local id = link.path:match("^/w/([%w%-]+)")
    if not id then return router.fail.not_applicable("not a video path") end

    local res = ctx.http.get("https://" .. link.host .. "/api/v1/videos/" .. id)
    if res.status == 404 then return router.fail.definitive("removed") end
    if res.status ~= 200 then return router.fail.transient("api " .. res.status) end

    local video = ctx.json.decode(res.body)
    return router.content({
      kind = "video",
      url = video.streamingPlaylists[1].files[1].fileUrl,
      title = video.name,
    })
  end,
})
```

Resolver fields: `name`; `run(link, ctx)`; `family` (string or list, see
below); `timeout` (a duration; when `run` doesn't return in time, the attempt
counts as a `transient` failure with reason `"timeout"`).

A chain entry is a resolver name, or `{ "name", options = { … } }`; the options
reach `run` as `ctx.opts` (used by `core.download` below).

Failure kinds decide what the chain does next:

| Constructor | Meaning | Chain continues with |
|---|---|---|
| `router.fail.not_applicable(reason)` | This resolver doesn't handle this link | the next resolver |
| `router.fail.transient(reason)` | Network error, timeout, rate limit, malformed response | the next resolver |
| `router.fail.definitive(reason)` | The service said no (gated, removed, unplayable) | the next resolver that shares no `family` with this one; a resolver without a family skips nothing beyond itself |

A thrown Lua error counts as `transient`. Events and traces see a failure as a
table: `{ kind = "transient" | "definitive" | "not_applicable", reason = "…", resolver = "…" }`.

Content that fails in the target before it is shown (see `content_shown` in
[Events](#events-routeronname-fn)) counts as a `definitive` failure of the
resolver that produced it, so the chain skips that resolver's family; it emits
`resolve.failed` like any other failure, with a reason starting with
`"content: "`. Failures after `content_shown` (a network drop in minute ten)
end the item without a retry or fallback.

### Core resolvers

| Name | Result |
|---|---|
| `core.download` | Downloads the link to a temporary file; content `{ url = "file://…", kind = … }`. `kind` from the response's Content-Type: `image/*` → `"image"`, `video/*` → `"video"`, `audio/*` → `"audio"`, `application/pdf` and `application/epub+zip` → `"document"`, anything else → no kind. Options (`{ "core.download", options = { keep = "1h" } }`): `keep`, default `"10m"`. The file is removed at the later of the opening process's exit and `keep` after the launch ([integration.md](integration.md#app-drivers)) |

## Content: `router.content(fields)`

What resolvers return and targets receive.

| Field | Meaning |
|---|---|
| `kind` | string used by `target` tables, `accepts` and events: `"video"`, `"audio"`, `"image"`, `"document"`, … |
| `url` | the main resource: stream URL, image URL, `file://` URL of a download |
| `extra` | list of additional resources with roles, e.g. `{ { role = "audio", url = … } }` |
| `title` | display title |
| `http_headers` | headers the target must send when fetching |
| `meta` | free-form table for the target and events (size, duration, `live`, …) |
| `source` | original link; set by the core, used for retries and the link's fallback |

A resolver can return a list with `router.content_list({ items = { … }, title = … })`
(a gallery, a carousel, a playlist). Driver targets open the items as one
playlist; per-item `opts.mode = "replace" | "append"` decides whether the list
replaces what the app is showing.

## Targets: `router.target(spec)`

A target is how content gets opened. Three forms:

```lua
-- A program started per item (no shell).
router.target({ name = "imv", exec = { "imv", "{path}" } })

-- A desktop entry, launched per the spec (field codes, activation token).
router.target({ name = "zathura", desktop = "org.pwmt.zathura" })

-- A managed app: a driver keeps one instance, reuses it, and reports events.
router.target({
  name = "player",
  driver = "mpv",
  options = { app_id = "mpv-video", args = { "--gpu-api=opengl" } },
})
```

Placeholders in `exec` argv elements:

| Placeholder | Value |
|---|---|
| `{url}` | the content's `url` |
| `{path}` | the local path of a `file://` content `url`; a target using it rejects other URLs (a `definitive` failure) |
| `{title}` | the content's title, or empty |

A placeholder is substituted inside its argv element and never splits it into
several elements; it may appear anywhere in the element (`"--url={url}"`).

Target fields for all three forms: `name`, `accepts`, `open_opts` (per-item
options for every item opened with this target; a route's `open_opts` and an
action's `opts` override them, in that order), `failure_grace` (`exec` and
`desktop` only). Desktop entry IDs may be written with or without `.desktop`.

`exec` and `desktop` targets count as failed if the process exits non-zero
within `failure_grace` (default `"2s"`, per target), and the route's fallback
runs. A `desktop` target expands `%f`/`%F` to the local path of `file://`
content and rejects other URLs; `%u`/`%U` get the URL.

`accepts = { "video", "audio" }` limits the content kinds a target opens; it is
checked only when the content has a kind. A rejection is a `definitive` failure
of the producing resolver.

Driver targets and their options are in [integration.md](integration.md#app-drivers).

## Actions

Handlers, `link.received` and `link.fallback` callbacks return actions.

| Constructor | Effect |
|---|---|
| `router.act.open(content_or_link, target, opts?)` | Open with a target; `opts` go to the target for this item |
| `router.act.resolve(chain, target, opts?)` | Run a resolver chain now, then open |
| `router.act.browser(link, desktop?)` | Open in the given desktop entry, else the link's fallback (`browser` from the config for http(s), else the original of the entry the link came through) |
| `router.act.exec(argv)` | Start a program (no shell) |
| `router.act.route(name)` | Hand the link to another named route |
| `router.act.notify(text)` | Show a desktop notification, nothing else |
| `router.act.none()` | Drop the link |

## Events: `router.on(name, fn)`

Callbacks receive an event table with named fields and a `ctx`:
`function(e, ctx) … end`. They run on the Lua thread in registration order. For
events whose return value is used, the first non-`nil` return wins and the
remaining callbacks for that event are skipped.

Core events:

| Event | `e` fields | Return value |
|---|---|---|
| `daemon.start` | — | ignored |
| `config.reloaded` | — | ignored |
| `link.received` | `link` | an action replaces routing entirely |
| `link.routed` | `link`, `route` | ignored |
| `resolve.failed` | `link`, `failure` | ignored |
| `link.fallback` | `link`, `reason` | an action replaces the route's fallback |
| `link.done` | `link`, `trace` | ignored |

Target events, emitted by drivers. Names are prefixed with the target's full
name (`"mpv-video.player:content_shown"`); `"*:content_shown"` listens to all
targets. Inside a plugin, an unprefixed target name refers to the plugin's own
target. Every target event carries `target`; events about an item also carry
`content` and `link`.

| Event | `e` fields | Return value |
|---|---|---|
| `<target>:started` | `target` | ignored |
| `<target>:content_started` | `target`, `content`, `link` | ignored |
| `<target>:content_shown` | `target`, `content`, `link` | ignored; ends the link's trace |
| `<target>:content_ended` | `target`, `content`, `link`, `reason` (`"eof"`, `"stop"`, `"error"`) | ignored; an `"error"` before `content_shown` re-enters the resolver chain |
| `<target>:idle` | `target` | `false` keeps an idle instance open |
| `<target>:<driver event>` | `target`, `content`, `link`, plus driver-specific fields | driver-specific (the mpv driver's `video_params` adds `w`, `h`, `dw`, `dh`; see [integration.md](integration.md#events)) |

## Values

### `link`

Read-only table passed to predicates, handlers, resolvers and events.

| Field | Example |
|---|---|
| `url` | `"https://youtube.com/shorts/j2Ky4AJ1OvA?si=abc"` |
| `scheme`, `host`, `port`, `path`, `fragment` | parsed; lowercase scheme and host |
| `query` | `{ si = "abc" }` (first value per key); `link.query_all` has lists |
| `opener` | `"telegram-desktop"` or `""` |
| `workspace` | the focused workspace when the link was received, from the compositor adapter (see `ctx.compositor.focused_workspace()`), or `nil` |
| `content_type`, `content_length`, `final_url` | set when the link was probed, else `nil` |
| `id` | daemon-assigned number, as shown by `link-router trace` |

## Services

Every function the core calls with a `ctx` (resolvers, handlers, event
callbacks, plugin `setup`) gets these. Services are plain functions
(`ctx.http.get(url)`); handles returned by services use method calls
(`ctx.target("player"):prepare()`). `ctx.env` and the compositor adapter
use the environment forwarded with the current link; in `setup` and
`daemon.start` they use the daemon's.

| Service | Call forms |
|---|---|
| `ctx.http` | `get(url, opts?)`, `post(url, body, opts?)` → `{ status, headers, body }`; `stream(url, opts?)` → iterator over body chunks, stop by breaking out of the loop. `opts`: `headers`, `timeout`, `follow_redirects`, `hedge` (duration after which an identical request is raced on a new connection) |
| `ctx.json` | `decode(string)`, `encode(value)`, `select(string, paths)` (extracts only the given paths without building the whole document in Lua; **pending** Q4) |
| `ctx.xml` | `decode(string)` → element tree; `select(string, path)` (for DASH manifests and similar) |
| `ctx.html` | `select(string, css_selector)` → list of `{ text, attrs }` |
| `ctx.cache` | `get(key)`, `set(key, value, ttl?)`; namespaced per plugin; persisted under `$XDG_CACHE_HOME/link-router/` when `ttl` is given |
| `ctx.exec` | `run(argv, opts?)` → `{ status, stdout, stderr }`; `spawn(argv)` → handle with `wait()` and `pid` |
| `ctx.fs` | `download(url, opts?)` → temporary file path; `opts.keep` is a duration |
| `ctx.probe(url)` | → `{ status, content_type, length, final_url }` (see [Probing](#probing)) |
| `ctx.net` | `preconnect(url, n?)`: open `n` connections to the URL's origin ahead of use; `warm_dns(hosts)`: resolve names in the background so a later lookup by another process hits the local DNS cache (only useful when the system runs a caching resolver) |
| `ctx.target(name)` | → handle, with methods called as `handle:method(…)`: `prepare()` (start the app early; no-op for non-driver targets), `command(args)`, `get(property)`, `set(property, value)`; driver-specific methods |
| `ctx.compositor` | `name` (field: `"hyprland"`, `"kwin"`, `"none"`); `focused_workspace()` → `{ id, name, special, monitor = { name, x, y, width, height, scale } }`; `move_window(selector, { workspace = <a table returned by focused_workspace(), or { id = … } / { name = … }> })`; `resize_window(selector, w, h)`; `fit_window(selector, { content_size, box, anchor, margin })`; `raw(request)` |
| `ctx.notify` | `send(summary, body?, opts?)` |
| `ctx.log` | `debug`, `info`, `warn`, `error` |
| `ctx.env` | `get(name)` |

A window `selector` is one of `{ target = "mpv-video.player" }` (the driver
supplies the pid), `{ app_id = "…" }`, `{ pid = n }`, or an adapter-specific
`{ address = "…" }`.

## Probing

Routes that match on `content_type` need to know what a URL serves. The core
probes a link at most once: a `HEAD`, or a ranged `GET` of the first bytes when
`HEAD` isn't supported, following redirects. The result is kept on the link
(`content_type`, `content_length`, `final_url`) for later routes and resolvers.

A probe costs a round trip, so the core only probes when the first route that
could still match has a `content_type` field; routes for known hosts should
come before content-type routes. Which routes get probed and whether a probe
runs in parallel with a speculative handler is **pending** Q6 in
[decisions.md](decisions.md#open-questions).

## Stubs

`link-router lua-stubs > ~/.config/link-router/router.meta.lua` writes LuaLS
annotations (`---@meta`, `---@class`, `---@alias` for event names), like
Hyprland's generated `hl.meta.lua`, so editors complete and type-check
`init.lua` and plugins.
