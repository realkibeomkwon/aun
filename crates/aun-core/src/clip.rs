//! System clipboard watch. Polls the OS change counter (one cheap call, no content read) and reads
//! the text only when it moved.

use std::hash::{Hash, Hasher};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

pub const MAX_TEXT: usize = 1 << 20;

#[cfg(target_os = "macos")]
fn change_count() -> Option<i64> {
    // NSPasteboard is usable off the main thread (arboard does the same).
    Some(objc2_app_kit::NSPasteboard::generalPasteboard().changeCount() as i64)
}

#[cfg(windows)]
fn change_count() -> Option<i64> {
    // SAFETY: no arguments, no preconditions.
    Some(unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() } as i64)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn change_count() -> Option<i64> {
    None
}

pub fn digest(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Runs forever. `incoming` carries text from peers to put on the clipboard; `changed` hears local copies.
pub fn run(incoming: Receiver<String>, mut changed: impl FnMut(String)) {
    // shortcut: 300 ms polling of the change counter; switch to AddClipboardFormatListener /
    // NSPasteboard notifications if wakeups ever show up in an energy profile.
    let tick = Duration::from_millis(300);
    let mut cb = None;
    let mut seq = change_count();
    let mut last = 0u64;
    loop {
        let remote = match incoming.recv_timeout(tick) {
            Ok(t) => Some(t),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if cb.is_none() {
            cb = arboard::Clipboard::new().ok();
        }
        let Some(c) = cb.as_mut() else { continue };
        if let Some(text) = remote {
            last = digest(&text);
            if c.set_text(text).is_err() {
                cb = None;
            }
            seq = change_count();
            continue;
        }
        let now = change_count();
        if now.is_some() && now == seq {
            continue;
        }
        let text = match c.get_text() {
            Ok(t) => t,
            // The copying app still holds the clipboard (common on Windows): try again next tick.
            Err(arboard::Error::ClipboardOccupied) => continue,
            Err(_) => {
                seq = now;
                continue;
            }
        };
        seq = now;
        let d = digest(&text);
        if d != last && !text.is_empty() && text.len() <= MAX_TEXT {
            last = d;
            changed(text);
        }
    }
}
