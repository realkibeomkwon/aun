//! System clipboard watch. Polls the OS change counter (one cheap call, no content read) and reads
//! the text only when it moved, and only when the copying app didn't mark it private.

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

/// Pasteboard types that password managers and other apps mark private data with
/// (nspasteboard.org): never sent to another device.
pub const MAC_SECRET: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

pub fn secret_types<'a>(mut types: impl Iterator<Item = &'a str>) -> bool {
    types.any(|t| MAC_SECRET.contains(&t))
}

#[cfg(target_os = "macos")]
fn types_of(pb: &objc2_app_kit::NSPasteboard) -> Vec<String> {
    pb.types().map(|a| a.iter().map(|t| t.to_string()).collect()).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn secret() -> bool {
    secret_types(types_of(&objc2_app_kit::NSPasteboard::generalPasteboard()).iter().map(String::as_str))
}

/// Windows: what the clipboard says about itself. `ExcludeClipboardContentFromMonitorProcessing`
/// present, or `CanIncludeInClipboardHistory` / `CanUploadToCloudClipboard` set to 0, is how
/// password managers ask clipboard history and cloud sync to skip a copy; Aun is both.
/// `None` for a flag that is present but couldn't be read.
pub fn secret_flags(exclude: bool, history: Option<Option<u32>>, cloud: Option<Option<u32>>) -> bool {
    // A flag that is there but unreadable counts as "no": better to miss one copy than leak it.
    let no = |f: Option<Option<u32>>| matches!(f, Some(None) | Some(Some(0)));
    exclude || no(history) || no(cloud)
}

#[cfg(windows)]
fn secret() -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    let format = |name: &str| {
        let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated name.
        unsafe { RegisterClipboardFormatW(w.as_ptr()) }
    };
    // SAFETY: plain queries; the clipboard is opened (and always closed) only to read two DWORDs,
    // each locked and unlocked around a size-checked read.
    unsafe {
        let (exclude, history, cloud) = (format("ExcludeClipboardContentFromMonitorProcessing"), format("CanIncludeInClipboardHistory"), format("CanUploadToCloudClipboard"));
        let has = |f: u32| f != 0 && IsClipboardFormatAvailable(f) != 0;
        if has(exclude) {
            return true;
        }
        if !has(history) && !has(cloud) {
            return false;
        }
        let opened = OpenClipboard(std::ptr::null_mut()) != 0;
        let read = |f: u32| -> Option<Option<u32>> {
            if !has(f) {
                return None;
            }
            if !opened {
                return Some(None);
            }
            let h = GetClipboardData(f);
            let p = GlobalLock(h) as *const u32;
            if p.is_null() || GlobalSize(h) < 4 {
                if !p.is_null() {
                    GlobalUnlock(h);
                }
                return Some(None);
            }
            let v = p.read_unaligned();
            GlobalUnlock(h);
            Some(Some(v))
        };
        let (h, c) = (read(history), read(cloud));
        if opened {
            CloseClipboard();
        }
        secret_flags(false, h, c)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn secret() -> bool {
    false
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
        if secret() {
            seq = now;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_marks_keep_a_copy_home() {
        assert!(!secret_flags(false, None, None), "an ordinary copy goes");
        assert!(!secret_flags(false, Some(Some(1)), Some(Some(1))), "flags that allow it");
        assert!(secret_flags(true, None, None), "ExcludeClipboardContentFromMonitorProcessing");
        assert!(secret_flags(false, Some(Some(0)), None), "CanIncludeInClipboardHistory = 0");
        assert!(secret_flags(false, None, Some(Some(0))), "CanUploadToCloudClipboard = 0");
        assert!(secret_flags(false, Some(None), None), "present but unreadable");
    }

    /// The real Windows clipboard: CI runners only (`cargo test` on a desk PC would clobber it).
    #[cfg(windows)]
    #[test]
    fn marked_windows_copies_are_secret() {
        use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
        use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
        if std::env::var_os("CI").is_none() {
            return;
        }
        let set = |marks: &[(&str, u32)]| {
            // SAFETY: the clipboard is opened, filled with fresh global blocks it takes over, and closed.
            unsafe {
                while OpenClipboard(std::ptr::null_mut()) == 0 {
                    std::thread::sleep(Duration::from_millis(20));
                }
                EmptyClipboard();
                for (name, v) in marks {
                    let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
                    let h = GlobalAlloc(GMEM_MOVEABLE, 4);
                    (GlobalLock(h) as *mut u32).write_unaligned(*v);
                    GlobalUnlock(h);
                    SetClipboardData(RegisterClipboardFormatW(w.as_ptr()), h);
                }
                CloseClipboard();
            }
        };
        set(&[("CanIncludeInClipboardHistory", 1)]);
        assert!(!secret());
        set(&[("ExcludeClipboardContentFromMonitorProcessing", 0)]);
        assert!(secret());
        set(&[("CanIncludeInClipboardHistory", 0)]);
        assert!(secret());
        set(&[("CanIncludeInClipboardHistory", 1), ("CanUploadToCloudClipboard", 0)]);
        assert!(secret());
        set(&[]);
    }

    /// On a named pasteboard of its own, so the user's clipboard is untouched.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_concealed_mac_copy_is_secret() {
        use objc2_app_kit::NSPasteboard;
        use objc2_foundation::NSString;
        let pb = NSPasteboard::pasteboardWithName(&NSString::from_str(&format!("dev.aun.test.{}", std::process::id())));
        let text = NSString::from_str("public.utf8-plain-text");
        pb.clearContents();
        pb.setString_forType(&NSString::from_str("hunter2"), &text);
        assert!(!secret_types(types_of(&pb).iter().map(String::as_str)), "plain text alone is not secret: {:?}", types_of(&pb));
        for mark in MAC_SECRET {
            pb.clearContents();
            pb.setString_forType(&NSString::from_str("hunter2"), &text);
            pb.setString_forType(&NSString::from_str(""), &NSString::from_str(mark));
            assert!(secret_types(types_of(&pb).iter().map(String::as_str)), "{mark}: {:?}", types_of(&pb));
        }
        // SAFETY: `releaseGlobally` takes no arguments; the test is done with this pasteboard.
        let () = unsafe { objc2::msg_send![&*pb, releaseGlobally] };
    }
}
