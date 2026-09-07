//! Presses Ctrl+V in whatever window has the keyboard, through the ydotool
//! daemon.
//!
//! Wayland has no portable way to type into another application. `ydotoold`
//! owns `/dev/uinput` and accepts raw `input_event` structs over a Unix
//! socket; each event is 24 bytes on 64-bit Linux: a zeroed `timeval`,
//! `type`, `code` and `value`.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_REPORT: u16 = 0;
const KEY_LEFTCTRL: u16 = 29;
const KEY_V: u16 = 47;

/// `$YDOTOOL_SOCKET`, then the per-user runtime directory, then `/tmp`.
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("YDOTOOL_SOCKET") {
        return PathBuf::from(path);
    }
    if let Some(runtime) = dirs::runtime_dir() {
        let candidate = runtime.join(".ydotool_socket");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("/tmp/.ydotool_socket")
}

pub fn available() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

pub fn ctrl_v() -> Result<()> {
    let path = socket_path();
    let mut socket = UnixStream::connect(&path)
        .with_context(|| format!("ydotool is not running (no socket at {})", path.display()))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    let steps = [
        (KEY_LEFTCTRL, 1),
        (KEY_V, 1),
        (KEY_V, 0),
        (KEY_LEFTCTRL, 0),
    ];
    for (code, value) in steps {
        socket.write_all(&event(EV_KEY, code, value))?;
        socket.write_all(&event(EV_SYN, SYN_REPORT, 0))?;
        std::thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}

pub fn event(kind: u16, code: u16, value: i32) -> [u8; 24] {
    let mut bytes = [0u8; 24];
    bytes[16..18].copy_from_slice(&kind.to_ne_bytes());
    bytes[18..20].copy_from_slice(&code.to_ne_bytes());
    bytes[20..24].copy_from_slice(&value.to_ne_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_what_the_ydotool_client_sends() {
        // Observed with strace: `ydotool key 42:1` writes these 24 bytes.
        let press_shift = event(EV_KEY, 42, 1);
        let mut expected = [0u8; 24];
        expected[16] = 0x01;
        expected[18] = 0x2a;
        expected[20] = 0x01;
        assert_eq!(press_shift, expected);
        assert_eq!(event(EV_SYN, SYN_REPORT, 0), [0u8; 24]);
    }
}
