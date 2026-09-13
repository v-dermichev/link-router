//! Hyprland adapter over its request socket (no `hyprctl` process): JSON
//! queries with `j/…`, Lua with `/eval` and `/dispatch`.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct Hypr {
    socket: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Workspace selector for `hl.dsp.window.move`: the open special workspace
    /// if any, else the active one.
    pub workspace: String,
}

#[derive(Debug, Clone)]
pub struct Client {
    pub address: String,
}

fn lua_str(s: &str) -> String {
    format!("{s:?}")
}

fn regex_escape(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_string() } else { format!("\\{c}") }).collect()
}

/// Largest size with the content's aspect ratio inside `box_size`.
pub fn fit(content: (u32, u32), box_size: (u32, u32)) -> (u32, u32) {
    let (cw, ch) = content;
    let (bw, bh) = box_size;
    if cw == 0 || ch == 0 {
        return (bw, bw * 9 / 16);
    }
    let aspect = cw as f64 / ch as f64;
    let mut w = bw as f64;
    let mut h = (w / aspect).round();
    if h > bh as f64 {
        h = bh as f64;
        w = (h * aspect).round();
    }
    (w as u32, h as u32)
}

impl Hypr {
    pub fn from_env(env: &BTreeMap<String, String>) -> Option<Self> {
        let sig = env.get("HYPRLAND_INSTANCE_SIGNATURE").cloned().or_else(|| std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok())?;
        let runtime = env.get("XDG_RUNTIME_DIR").cloned().or_else(|| std::env::var("XDG_RUNTIME_DIR").ok())?;
        let socket = PathBuf::from(runtime).join("hypr").join(sig).join(".socket.sock");
        socket.exists().then_some(Self { socket })
    }

    async fn request(&self, cmd: &str) -> Result<String> {
        let mut s = UnixStream::connect(&self.socket).await?;
        s.write_all(cmd.as_bytes()).await?;
        let mut out = String::new();
        s.read_to_string(&mut out).await?;
        Ok(out)
    }

    async fn json(&self, what: &str) -> Result<Value> {
        Ok(serde_json::from_str(&self.request(&format!("j/{what}")).await?)?)
    }

    pub async fn focused_monitor(&self) -> Result<Monitor> {
        let monitors = self.json("monitors").await?;
        let m = monitors
            .as_array()
            .and_then(|a| a.iter().find(|m| m.get("focused").and_then(Value::as_bool) == Some(true)))
            .ok_or_else(|| anyhow!("no focused monitor"))?;
        let int = |k: &str| m.get(k).and_then(Value::as_i64).unwrap_or(0) as i32;
        let scale = m.get("scale").and_then(Value::as_f64).unwrap_or(1.0).max(0.1);
        let special = m.pointer("/specialWorkspace/name").and_then(Value::as_str).unwrap_or("");
        let workspace = if !special.is_empty() {
            special.to_string()
        } else {
            let name = m.pointer("/activeWorkspace/name").and_then(Value::as_str).unwrap_or("1");
            let id = m.pointer("/activeWorkspace/id").and_then(Value::as_i64).unwrap_or(0);
            if name == id.to_string() { name.to_string() } else { format!("name:{name}") }
        };
        Ok(Monitor {
            x: int("x"),
            y: int("y"),
            width: (int("width") as f64 / scale).round() as i32,
            height: (int("height") as f64 / scale).round() as i32,
            workspace,
        })
    }

    /// Size and monitor-local position for windows of `app_id` mapped from now on.
    pub async fn set_rule(&self, app_id: &str, size: (u32, u32), local: (i32, i32)) -> Result<()> {
        let cmd = format!(
            "/eval hl.window_rule({{ name = \"link-router-player\", match = {{ class = {} }}, float = true, size = \"{} {}\", move = \"{} {}\" }})",
            lua_str(&format!("^({})$", regex_escape(app_id))),
            size.0, size.1, local.0, local.1
        );
        self.request(&cmd).await.map(|_| ())
    }

    pub async fn client(&self, app_id: &str, pid: Option<u32>) -> Result<Option<Client>> {
        let clients = self.json("clients").await?;
        Ok(clients.as_array().and_then(|a| {
            a.iter()
                .find(|c| {
                    c.get("class").and_then(Value::as_str) == Some(app_id)
                        && pid.map(|p| c.get("pid").and_then(Value::as_u64) == Some(p as u64)).unwrap_or(true)
                })
                .map(|c| Client { address: c.get("address").and_then(Value::as_str).unwrap_or_default().to_string() })
        }))
    }

    pub async fn resize_move(&self, address: &str, size: (u32, u32), absolute: (i32, i32)) -> Result<()> {
        let window = lua_str(&format!("address:{address}"));
        self.request(&format!("/dispatch hl.dsp.window.resize({{ x = {}, y = {}, window = {window} }})", size.0, size.1)).await?;
        self.request(&format!("/dispatch hl.dsp.window.move({{ x = {}, y = {}, window = {window} }})", absolute.0, absolute.1)).await?;
        Ok(())
    }

    pub async fn move_to_workspace(&self, address: &str, workspace: &str) -> Result<()> {
        let cmd = format!(
            "/dispatch hl.dsp.window.move({{ workspace = {}, window = {} }})",
            lua_str(workspace),
            lua_str(&format!("address:{address}"))
        );
        self.request(&cmd).await.map(|_| ())
    }
}

/// Bottom-right anchored position of a window of `size` on `monitor`, as
/// (monitor-local, absolute) coordinates.
pub fn anchor(monitor: &Monitor, size: (u32, u32), margin: (i32, i32)) -> ((i32, i32), (i32, i32)) {
    let lx = monitor.width - margin.0 - size.0 as i32;
    let ly = monitor.height - margin.1 - size.1 as i32;
    ((lx, ly), (monitor.x + lx, monitor.y + ly))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_box() {
        assert_eq!(fit((1920, 1080), (960, 720)), (960, 540));
        assert_eq!(fit((720, 1280), (960, 720)), (405, 720));
        assert_eq!(fit((640, 480), (960, 720)), (960, 720));
    }

    #[test]
    fn anchors_bottom_right() {
        let m = Monitor { x: 1920, y: 0, width: 1920, height: 1080, workspace: "1".into() };
        assert_eq!(anchor(&m, (960, 540), (35, 25)), ((925, 515), (2845, 515)));
    }
}
