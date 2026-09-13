//! Minimal Wayland client that lists the compositor's globals, to pick an mpv
//! output that suits the session before mpv starts.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

fn socket(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let display = env.get("WAYLAND_DISPLAY").filter(|d| !d.is_empty())?;
    if display.starts_with('/') {
        return Some(PathBuf::from(display));
    }
    Some(PathBuf::from(env.get("XDG_RUNTIME_DIR")?).join(display))
}

fn message(object: u32, opcode: u16, new_id: u32) -> [u8; 12] {
    let mut m = [0u8; 12];
    m[..4].copy_from_slice(&object.to_ne_bytes());
    m[4..8].copy_from_slice(&((12u32 << 16) | opcode as u32).to_ne_bytes());
    m[8..].copy_from_slice(&new_id.to_ne_bytes());
    m
}

/// Interface names the compositor advertises, or `None` when it can't be asked.
pub fn globals(env: &BTreeMap<String, String>) -> Option<Vec<String>> {
    const DISPLAY: u32 = 1;
    const REGISTRY: u32 = 2;
    const CALLBACK: u32 = 3;
    let mut s = UnixStream::connect(socket(env)?).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    let mut request = Vec::with_capacity(24);
    request.extend_from_slice(&message(DISPLAY, 1, REGISTRY)); // wl_display.get_registry
    request.extend_from_slice(&message(DISPLAY, 0, CALLBACK)); // wl_display.sync
    s.write_all(&request).ok()?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut names = Vec::new();
    loop {
        while buf.len() >= 8 {
            let object = u32::from_ne_bytes(buf[..4].try_into().unwrap());
            let word = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
            let (size, opcode) = ((word >> 16) as usize, word & 0xffff);
            if size < 8 {
                return None;
            }
            if buf.len() < size {
                break;
            }
            let body = &buf[8..size];
            match (object, opcode) {
                (REGISTRY, 0) if body.len() >= 8 => {
                    // global(name: uint, interface: string, version: uint)
                    let len = u32::from_ne_bytes(body[4..8].try_into().unwrap()) as usize;
                    if let Some(bytes) = body.get(8..8 + len.saturating_sub(1)) {
                        names.push(String::from_utf8_lossy(bytes).into_owned());
                    }
                }
                (CALLBACK, 0) => return Some(names),
                (DISPLAY, 0) => return None,
                _ => {}
            }
            buf.drain(..size);
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Whether clients can hand GPU buffers to the compositor. Without it (software
/// renderers such as wlroots' pixman) mpv's GPU outputs render on the CPU
/// through llvmpipe, which costs several times more than shared-memory frames.
pub fn shares_gpu_buffers(globals: &[String]) -> bool {
    globals.iter().any(|g| g == "zwp_linux_dmabuf_v1" || g == "wl_drm")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gpu_buffer_sharing() {
        let g = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert!(shares_gpu_buffers(&g(&["wl_shm", "zwp_linux_dmabuf_v1"])));
        assert!(!shares_gpu_buffers(&g(&["wl_shm", "xdg_wm_base"])));
    }
}
