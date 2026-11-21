//! System clipboard watch. Polls the OS change counter (one cheap call, no content read) and reads
//! the text only when it moved, and only when the copying app didn't mark it private.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

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

/// Apps that copy passwords as plain text with no private mark: a copy made while one of them
/// is in front counts as secret. Apple's Passwords app, its window and its menu bar item.
pub const MAC_SECRET_APPS: [&str; 2] = ["com.apple.Passwords", "com.apple.Passwords.MenuBarExtra"];

pub fn secret_app(bundle_id: Option<&str>) -> bool {
    bundle_id.is_some_and(|b| MAC_SECRET_APPS.contains(&b))
}

/// shortcut: the app in front when the poll sees the change, not the app that copied; a copy
/// from a menu bar item over another app is missed. Upgrade if a password manager does that.
#[cfg(target_os = "macos")]
fn front_app() -> Option<String> {
    let app = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication()?;
    app.bundleIdentifier().map(|b| b.to_string())
}

#[cfg(target_os = "macos")]
fn types_of(pb: &objc2_app_kit::NSPasteboard) -> Vec<String> {
    pb.types().map(|a| a.iter().map(|t| t.to_string()).collect()).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn secret() -> bool {
    secret_types(types_of(&objc2_app_kit::NSPasteboard::generalPasteboard()).iter().map(String::as_str)) || secret_app(front_app().as_deref())
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

pub fn digest(s: &(impl Hash + ?Sized)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// What moves between clipboards.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    /// An image as PNG: lossless and far smaller than the RGBA a clipboard holds.
    Png(Vec<u8>),
}

/// A decoded image may take at most this much memory (about 5800 × 5800): a small PNG can claim
/// a huge canvas, and peers are only trusted so far.
pub const MAX_PIXELS_BYTES: usize = 128 << 20;

pub fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(rgba.len() / 4);
    let mut e = png::Encoder::new(&mut out, width as u32, height as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    // fdeflate: several hundred MB/s and still about as small as zlib's fast levels.
    e.set_compression(png::Compression::Fast);
    let mut w = e.write_header().ok()?;
    w.write_image_data(rgba).ok()?;
    w.finish().ok()?;
    Some(out)
}

/// RGBA8 rows of a PNG from a peer, or None if it is broken, too big, or not 8-bit RGB(A)/gray.
pub fn decode_png(png: &[u8], max_bytes: usize) -> Option<(usize, usize, Vec<u8>)> {
    let mut d = png::Decoder::new_with_limits(std::io::Cursor::new(png), png::Limits { bytes: max_bytes });
    d.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut r = d.read_info().ok()?;
    // png's own limit covers its working buffers, not the image we allocate here.
    let size = r.output_buffer_size().filter(|&n| n <= max_bytes)?;
    let mut buf = vec![0; size];
    let info = r.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((info.width as usize, info.height as usize, buf))
}

/// Runs forever. `incoming` carries what peers copied, for this clipboard; `changed` hears local
/// copies (images from a thread of their own, so a big one doesn't hold up the clipboard).
/// `share_secrets` says whether copies marked private go too.
pub fn run(incoming: Receiver<Content>, share_secrets: impl Fn() -> bool, changed: impl Fn(Content) + Send + Sync + 'static) {
    // shortcut: 300 ms polling of the change counter; switch to AddClipboardFormatListener /
    // NSPasteboard notifications if wakeups ever show up in an energy profile.
    let tick = Duration::from_millis(300);
    let changed = Arc::new(changed);
    // Bumped by every local copy: an image still encoding when a newer copy comes is dropped.
    let latest = Arc::new(AtomicU64::new(0));
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
        if let Some(content) = remote {
            let set = match content {
                Content::Text(text) => {
                    last = digest(&text);
                    c.set_text(text)
                }
                Content::Png(png) => {
                    let Some((width, height, rgba)) = decode_png(&png, MAX_PIXELS_BYTES) else { continue };
                    last = digest(&rgba[..]);
                    c.set_image(arboard::ImageData { width, height, bytes: rgba.into() })
                }
            };
            if set.is_err() {
                cb = None;
            }
            seq = change_count();
            continue;
        }
        let now = change_count();
        if now.is_some() && now == seq {
            continue;
        }
        if !share_secrets() && secret() {
            seq = now;
            continue;
        }
        let text = match c.get_text() {
            Ok(t) => Some(t),
            // The copying app still holds the clipboard (common on Windows): try again next tick.
            Err(arboard::Error::ClipboardOccupied) => continue,
            Err(_) => None,
        };
        seq = now;
        let generation = latest.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(text) = text {
            let d = digest(&text);
            if d != last && !text.is_empty() && text.len() <= MAX_TEXT {
                last = d;
                changed(Content::Text(text));
            }
            continue;
        }
        let Ok(img) = c.get_image() else { continue };
        let d = digest(&img.bytes[..]);
        if d == last || img.bytes.is_empty() {
            continue;
        }
        last = d;
        let (changed, latest) = (changed.clone(), latest.clone());
        let _ = std::thread::Builder::new().name("aun-png".into()).spawn(move || {
            let t = Instant::now();
            let png = encode_png(img.width, img.height, &img.bytes);
            crate::log(format_args!("clipboard image {}x{} to {} KB PNG in {} ms", img.width, img.height, png.as_ref().map_or(0, |p| p.len() >> 10), t.elapsed().as_millis()));
            if let Some(png) = png
                && latest.load(Ordering::Relaxed) == generation
            {
                changed(Content::Png(png));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screenshot(w: usize, h: usize) -> Vec<u8> {
        // Flat UI colours with some text-like detail, as a screenshot has.
        (0..w * h).flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let ink = (x / 3 + y / 7) % 11 == 0 && y % 40 < 14;
            if ink { [20, 20, 20, 255] } else { [((x / 64) as u8).wrapping_mul(9), 230, 240 - (y / 90) as u8, 255] }
        }).collect()
    }

    #[test]
    fn images_survive_the_png_round_trip() {
        let (w, h) = (2880, 1800);
        let rgba = screenshot(w, h);
        let t = Instant::now();
        let png = encode_png(w, h, &rgba).unwrap();
        let enc = t.elapsed();
        let t = Instant::now();
        let (dw, dh, back) = decode_png(&png, MAX_PIXELS_BYTES).unwrap();
        println!("{w}x{h}: {} KB RGBA -> {} KB PNG, encode {enc:?}, decode {:?}", rgba.len() >> 10, png.len() >> 10, t.elapsed());
        assert_eq!((dw, dh), (w, h));
        assert!(back == rgba, "pixels differ");
        assert!(png.len() < crate::wire::MAX_IMAGE);
    }

    /// `AUN_SHOT=shot.png cargo test --release -p aun-core --lib -- --ignored --nocapture png_speed`
    #[test]
    #[ignore]
    fn png_speed_on_a_real_screenshot() {
        let png = std::fs::read(std::env::var("AUN_SHOT").expect("AUN_SHOT=path to a PNG")).unwrap();
        let (w, h, rgba) = decode_png(&png, usize::MAX).unwrap();
        let t = Instant::now();
        let ours = encode_png(w, h, &rgba).unwrap();
        let enc = t.elapsed();
        let t = Instant::now();
        decode_png(&ours, MAX_PIXELS_BYTES).unwrap();
        println!("{w}x{h}: {} KB RGBA -> {} KB PNG (source {} KB), encode {enc:?}, decode {:?}", rgba.len() >> 10, ours.len() >> 10, png.len() >> 10, t.elapsed());
    }

    /// Run alone (`-- --ignored --exact clip::tests::poll_alone`) and watch the process with
    /// `top -stats csw,idlew`: what the 300 ms poll costs by itself.
    #[test]
    #[ignore]
    fn poll_alone() {
        let (_tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || run(rx, || false, |_| {}));
        std::thread::sleep(Duration::from_secs(40));
    }

    #[test]
    #[ignore]
    fn sleep_alone() {
        std::thread::sleep(Duration::from_secs(40));
    }

    #[test]
    fn a_png_claiming_a_huge_canvas_is_refused() {
        let png = encode_png(1024, 1024, &vec![0; 1024 * 1024 * 4]).unwrap();
        assert!(png.len() < 64 << 10, "a blank canvas compresses to almost nothing");
        assert!(decode_png(&png, 1 << 20).is_none(), "4 MiB of pixels past a 1 MiB limit");
        assert!(decode_png(&png, 8 << 20).is_some());
        assert!(decode_png(b"\x89PNG not really", MAX_PIXELS_BYTES).is_none());
    }

    #[test]
    fn private_marks_keep_a_copy_home() {
        assert!(!secret_flags(false, None, None), "an ordinary copy goes");
        assert!(!secret_flags(false, Some(Some(1)), Some(Some(1))), "flags that allow it");
        assert!(secret_flags(true, None, None), "ExcludeClipboardContentFromMonitorProcessing");
        assert!(secret_flags(false, Some(Some(0)), None), "CanIncludeInClipboardHistory = 0");
        assert!(secret_flags(false, None, Some(Some(0))), "CanUploadToCloudClipboard = 0");
        assert!(secret_flags(false, Some(None), None), "present but unreadable");
    }

    #[test]
    fn a_copy_made_in_apple_passwords_is_secret() {
        // Apple's Passwords app puts plain text only, no private mark (seen on the device, 2026-11-21).
        assert!(secret_app(Some("com.apple.Passwords")));
        assert!(secret_app(Some("com.apple.Passwords.MenuBarExtra")));
        assert!(!secret_app(Some("com.google.Chrome")));
        assert!(!secret_app(None), "no frontmost app");
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
