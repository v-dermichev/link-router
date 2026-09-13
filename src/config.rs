//! Config from `init.lua`. The MVP implements `router.config` and
//! `router.use("mpv-video", opts)`; the rest of the `router` API is stubbed.

use anyhow::{Context, Result};
use mlua::{Lua, LuaSerdeExt, Value};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::paths;

#[derive(Debug, Clone)]
pub struct Config {
    /// `None` keeps the daemon resident.
    pub idle_exit: Option<Duration>,
    pub video: VideoConfig,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub enabled: bool,
    pub app_id: String,
    pub args: Vec<String>,
    /// `Some(value)` sets, `None` unsets.
    pub env: BTreeMap<String, Option<String>>,
    pub stream_options: Vec<String>,
    pub max_resolution: u32,
    pub max_fps: u32,
    pub buffer_secs: f64,
    pub network_timeout: u32,
    pub box_size: (u32, u32),
    pub margin: (i32, i32),
    pub follow_focus: bool,
    pub youtube: bool,
    pub instagram: bool,
    /// Path extensions (lower case, no dot) played as they are; empty disables.
    pub direct: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { idle_exit: Some(Duration::from_secs(30 * 60)), video: VideoConfig::default(), warnings: Vec::new() }
    }
}

/// `.ts` is left out: it is far more often TypeScript source than a video.
pub const DIRECT_EXTENSIONS: &[&str] = &["mp4", "m4v", "webm", "mkv", "mov", "ogv", "avi", "flv", "3gp", "m3u8"];

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: "link-router-mpv".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            stream_options: vec!["tcp_nodelay=1".into()],
            max_resolution: 1080,
            max_fps: 60,
            buffer_secs: 1.0,
            network_timeout: 8,
            box_size: (960, 720),
            margin: (35, 25),
            follow_focus: true,
            youtube: true,
            instagram: true,
            direct: DIRECT_EXTENSIONS.iter().map(|e| e.to_string()).collect(),
        }
    }
}

fn duration(v: &Json) -> Option<Option<Duration>> {
    match v {
        Json::Bool(false) => Some(None),
        Json::Number(n) => n.as_f64().map(|s| Some(Duration::from_secs_f64(s))),
        Json::String(s) => {
            let (num, unit) = s.trim().split_at(s.trim().find(|c: char| c.is_alphabetic()).unwrap_or(s.len()));
            let n: f64 = num.parse().ok()?;
            let secs = match unit {
                "ms" => n / 1000.0,
                "" | "s" => n,
                "m" => n * 60.0,
                "h" => n * 3600.0,
                "d" => n * 86400.0,
                _ => return None,
            };
            Some(Some(Duration::from_secs_f64(secs)))
        }
        _ => None,
    }
}

impl Config {
    pub fn load() -> Config {
        let mut cfg = Config::default();
        let path = paths::config_file();
        let Ok(source) = fs::read_to_string(&path) else {
            return cfg;
        };
        if let Err(e) = cfg.apply_lua(&source) {
            cfg.warnings.push(format!("{}: {e:#}; using defaults", path.display()));
            let mut fallback = Config::default();
            fallback.warnings = cfg.warnings;
            return fallback;
        }
        cfg
    }

    fn apply_lua(&mut self, source: &str) -> Result<()> {
        let lua = Lua::new();
        let configs: Arc<Mutex<Vec<Json>>> = Arc::default();
        let uses: Arc<Mutex<Vec<(String, Json)>>> = Arc::default();
        let stubbed: Arc<Mutex<Vec<String>>> = Arc::default();
        let router = lua.create_table()?;

        let c = configs.clone();
        router.set("config", lua.create_function(move |lua, t: Value| {
            c.lock().unwrap().push(lua.from_value(t)?);
            Ok(())
        })?)?;
        let u = uses.clone();
        router.set("use", lua.create_function(move |lua, (name, opts): (String, Option<Value>)| {
            let opts = match opts {
                Some(v) => lua.from_value(v)?,
                None => Json::Object(Default::default()),
            };
            u.lock().unwrap().push((name, opts));
            Ok(())
        })?)?;
        for name in ["route", "route_update", "route_remove", "resolver", "resolver_update", "resolver_remove", "target", "target_update", "target_remove", "on", "plugin"] {
            let s = stubbed.clone();
            router.set(name, lua.create_function(move |_, _: mlua::MultiValue| {
                s.lock().unwrap().push(name.to_string());
                Ok(())
            })?)?;
        }
        lua.globals().set("router", router)?;
        lua.load(source).set_name("init.lua").exec().context("running init.lua")?;

        for c in configs.lock().unwrap().iter() {
            if let Some(v) = c.pointer("/daemon/idle_exit").and_then(duration) {
                self.idle_exit = v;
            }
        }
        for (name, opts) in uses.lock().unwrap().iter() {
            match name.as_str() {
                "mpv-video" => {
                    self.video.enabled = true;
                    self.video.apply(opts);
                }
                other => self.warnings.push(format!("plugin {other:?} is not available in this build")),
            }
        }
        let stubbed = stubbed.lock().unwrap();
        if !stubbed.is_empty() {
            self.warnings.push(format!("not implemented yet, ignored: router.{}", stubbed.join(", router.")));
        }
        Ok(())
    }
}

impl VideoConfig {
    fn apply(&mut self, o: &Json) {
        let str_list = |v: &Json| -> Option<Vec<String>> {
            v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        };
        if let Some(v) = o.pointer("/player/app_id").and_then(Json::as_str) {
            self.app_id = v.into();
        }
        if let Some(v) = o.pointer("/player/args").and_then(str_list) {
            self.args = v;
        }
        if let Some(env) = o.pointer("/player/env").and_then(Json::as_object) {
            for (k, v) in env {
                match v {
                    Json::Bool(false) => {
                        self.env.insert(k.clone(), None);
                    }
                    Json::String(s) => {
                        self.env.insert(k.clone(), Some(s.clone()));
                    }
                    _ => {}
                }
            }
        }
        if let Some(v) = o.pointer("/player/stream_options").and_then(str_list) {
            self.stream_options = v;
        }
        if let Some(v) = o.pointer("/player/network_timeout").and_then(duration).flatten() {
            self.network_timeout = v.as_secs().max(1) as u32;
        }
        if let Some(v) = o.pointer("/quality/max_resolution").and_then(Json::as_u64) {
            self.max_resolution = v as u32;
        }
        if let Some(v) = o.pointer("/quality/max_fps").and_then(Json::as_u64) {
            self.max_fps = v as u32;
        }
        if let Some(v) = o.pointer("/quality/buffer").and_then(duration) {
            self.buffer_secs = v.map(|d| d.as_secs_f64()).unwrap_or(0.0);
        }
        if let Some(b) = o.pointer("/window/box").and_then(Json::as_array) {
            if let (Some(w), Some(h)) = (b.first().and_then(Json::as_u64), b.get(1).and_then(Json::as_u64)) {
                self.box_size = (w as u32, h as u32);
            }
        }
        if let Some(m) = o.pointer("/window/margin").and_then(Json::as_array) {
            if let (Some(x), Some(y)) = (m.first().and_then(Json::as_i64), m.get(1).and_then(Json::as_i64)) {
                self.margin = (x as i32, y as i32);
            }
        }
        if let Some(v) = o.pointer("/window/follow_focus").and_then(Json::as_bool) {
            self.follow_focus = v;
        }
        if let Some(v) = o.pointer("/sites/youtube").and_then(Json::as_bool) {
            self.youtube = v;
        }
        if let Some(v) = o.pointer("/sites/instagram").and_then(Json::as_bool) {
            self.instagram = v;
        }
        match o.pointer("/sites/direct") {
            Some(Json::Bool(false)) => self.direct.clear(),
            Some(v @ Json::Array(_)) => {
                self.direct = str_list(v).unwrap_or_default().iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plugin_options() {
        let mut cfg = Config::default();
        cfg.apply_lua(
            r#"
            router.config({ daemon = { idle_exit = false } })
            router.use("mpv-video", {
              player = { args = { "--gpu-api=opengl" }, env = { LIBVA_DRIVER_NAME = "iHD", __GLX_VENDOR_LIBRARY_NAME = false } },
              quality = { max_resolution = 720, buffer = "2s" },
              sites = { direct = { ".MP4", "webm" } },
            })
            router.route({ name = "x" })
            "#,
        )
        .unwrap();
        assert!(cfg.idle_exit.is_none());
        assert_eq!(cfg.video.direct, vec!["mp4", "webm"]);
        assert_eq!(cfg.video.args, vec!["--gpu-api=opengl"]);
        assert_eq!(cfg.video.env.get("LIBVA_DRIVER_NAME"), Some(&Some("iHD".into())));
        assert_eq!(cfg.video.env.get("__GLX_VENDOR_LIBRARY_NAME"), Some(&None));
        assert_eq!(cfg.video.max_resolution, 720);
        assert_eq!(cfg.video.buffer_secs, 2.0);
        assert!(cfg.warnings.iter().any(|w| w.contains("router.route")));
    }

    #[test]
    fn later_use_overrides_only_given_options() {
        let mut cfg = Config::default();
        cfg.apply_lua(
            r#"
            router.use("mpv-video", { quality = { max_resolution = 720 } })
            router.use("mpv-video", { sites = { youtube = true, instagram = false, direct = false } })
            "#,
        )
        .unwrap();
        assert_eq!(cfg.video.max_resolution, 720);
        assert!(cfg.video.youtube && !cfg.video.instagram && cfg.video.direct.is_empty());
    }
}
