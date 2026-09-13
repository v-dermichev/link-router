//! sway adapter over its IPC socket (`$SWAYSOCK`, i3 IPC framing): a
//! `for_window` rule keyed to the player's PID places a new window before it
//! maps, and criteria commands move a running one. sway can't remove
//! `for_window` rules, so each new player leaves one inert rule behind for the
//! rest of the sway session. The player is borderless there: sway adds its
//! title bar around the client's content after the rule has run, which would
//! push the window past the anchored corner.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::hypr::Monitor;

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;

pub struct Sway {
    socket: PathBuf,
}

/// A string argument for sway's command parser.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Sway {
    pub fn from_env(env: &BTreeMap<String, String>) -> Option<Self> {
        let socket = PathBuf::from(env.get("SWAYSOCK").cloned().or_else(|| std::env::var("SWAYSOCK").ok())?);
        socket.exists().then_some(Self { socket })
    }

    async fn request(&self, kind: u32, payload: &str) -> Result<Value> {
        let mut s = UnixStream::connect(&self.socket).await?;
        let mut msg = Vec::with_capacity(14 + payload.len());
        msg.extend_from_slice(MAGIC);
        msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        msg.extend_from_slice(&kind.to_ne_bytes());
        msg.extend_from_slice(payload.as_bytes());
        s.write_all(&msg).await?;
        let mut header = [0u8; 14];
        s.read_exact(&mut header).await?;
        if &header[..6] != MAGIC {
            bail!("not a sway IPC reply");
        }
        let len = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    async fn run(&self, command: &str) -> Result<()> {
        let reply = self.request(RUN_COMMAND, command).await?;
        let failed: Vec<&str> = reply
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|r| r.get("success").and_then(Value::as_bool) != Some(true))
                    .map(|r| r.get("error").and_then(Value::as_str).unwrap_or("failed"))
                    .collect()
            })
            .unwrap_or_default();
        if !failed.is_empty() {
            bail!("sway: {command}: {}", failed.join("; "));
        }
        Ok(())
    }

    /// The focused workspace's usable area (without bars) in layout coordinates.
    pub async fn focused_workspace(&self) -> Result<Monitor> {
        let workspaces = self.request(GET_WORKSPACES, "").await?;
        let ws = workspaces
            .as_array()
            .and_then(|a| a.iter().find(|w| w.get("focused").and_then(Value::as_bool) == Some(true)))
            .ok_or_else(|| anyhow!("no focused workspace"))?;
        let int = |k: &str| ws.pointer(&format!("/rect/{k}")).and_then(Value::as_i64).unwrap_or(0) as i32;
        Ok(Monitor {
            x: int("x"),
            y: int("y"),
            width: int("width"),
            height: int("height"),
            workspace: ws.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
        })
    }

    /// Floats, sizes and places the window of `pid` when it maps.
    pub async fn rule_for_pid(&self, pid: u32, size: (u32, u32), absolute: (i32, i32)) -> Result<()> {
        // Quoted: over IPC sway splits unquoted command lists at commas, which
        // would run everything after the first command immediately.
        self.run(&format!(
            "for_window [pid={pid}] \"floating enable, border none, resize set {} {}, move absolute position {} {}\"",
            size.0, size.1, absolute.0, absolute.1
        ))
        .await
    }

    /// Resizes and moves the running window of `pid`.
    pub async fn resize_move(&self, pid: u32, size: (u32, u32), absolute: (i32, i32)) -> Result<()> {
        self.run(&format!(
            "[pid={pid}] floating enable, border none, resize set {} {}, move absolute position {} {}",
            size.0, size.1, absolute.0, absolute.1
        ))
        .await
    }

    pub async fn move_to_workspace(&self, pid: u32, workspace: &str) -> Result<()> {
        self.run(&format!("[pid={pid}] move container to workspace {}", quote(workspace))).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_workspace_names() {
        assert_eq!(quote(r#"2: web "x"\"#), r#""2: web \"x\"\\""#);
    }
}
