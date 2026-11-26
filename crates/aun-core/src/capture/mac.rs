//! macOS: the event tap behind [`super::Cross`]. A session tap on its own thread's run loop.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, mpsc};

use super::{REARM, State, Why};
use crate::kvm::{self, Captured};

type Ref = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

type Callback = extern "C" fn(proxy: Ref, kind: u32, event: Ref, user: *mut c_void) -> Ref;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, callback: Callback, user: *mut c_void) -> Ref;
    fn CGEventTapEnable(tap: Ref, enable: bool);
    fn CGEventCreate(source: Ref) -> Ref;
    fn CGEventGetIntegerValueField(e: Ref, field: u32) -> i64;
    fn CGEventGetFlags(e: Ref) -> u64;
    fn CGEventGetLocation(e: Ref) -> Point;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
    fn CGWarpMouseCursorPosition(p: Point) -> i32;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: Ref, port: Ref, order: isize) -> Ref;
    fn CFMachPortInvalidate(port: Ref);
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopAddSource(rl: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: Ref);
    fn CFRelease(r: Ref);
    static kCFRunLoopCommonModes: Ref;
}

const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const ACTIVE: u32 = 0;
// Event types.
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const SCROLL: u32 = 22;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;
const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const DISABLED_BY_USER: u32 = 0xFFFF_FFFF;
// Event fields.
const CLICK_STATE: u32 = 1;
const BUTTON_NUMBER: u32 = 3;
const DELTA_X: u32 = 4;
const DELTA_Y: u32 = 5;
const KEYCODE: u32 = 9;
const SCROLL_POINT_Y: u32 = 96;
const SCROLL_POINT_X: u32 = 97;
const SOURCE_USER_DATA: u32 = 42;
/// Shift, Control, Option, Command (`kCGEventFlagMask*`): none may be down as a crossing starts.
const MODIFIERS: u64 = 0x1E_0000;
const CAPS_LOCK: u16 = 0x39;

/// The macOS part of [`State`].
pub(super) struct Shared {
    tap: AtomicPtr<c_void>,
    /// The input source shortcuts, as wire key and modifiers ([`kvm::input_source_keys`]): sent
    /// as 한/영. Read once as the tap starts.
    lang: Vec<(u16, u64)>,
}

impl Shared {
    pub(super) fn new() -> Shared {
        Shared { tap: AtomicPtr::new(std::ptr::null_mut()), lang: kvm::input_source_keys() }
    }
}

/// Where the cursor is now, in global display points.
pub(super) fn cursor() -> (f64, f64) {
    // SAFETY: an event made only to read the location, released at once.
    unsafe {
        let e = CGEventCreate(std::ptr::null_mut());
        if e.is_null() {
            return (0.0, 0.0);
        }
        let p = CGEventGetLocation(e);
        CFRelease(e);
        (p.x, p.y)
    }
}

/// The cursor back with the mouse, at `(x, y)`.
pub(super) fn give_back((x, y): (f64, f64)) {
    // SAFETY: plain calls taking values.
    unsafe {
        CGWarpMouseCursorPosition(Point { x, y });
        CGAssociateMouseAndMouseCursorPosition(true);
    }
}

/// The wire modifiers held, from an event's per-side flag bits (see [`kvm::escape`]).
fn mods(flags: u64) -> u8 {
    [0x3B, 0x38, 0x3A, 0x37, 0x3E, 0x3C, 0x3D, 0x36].iter().enumerate().fold(0, |m, (i, &code)| m | (u8::from(kvm::mac_modifier(code, flags).is_some_and(|k| k.1)) << i))
}

extern "C" fn tap(_proxy: Ref, kind: u32, e: Ref, user: *mut c_void) -> Ref {
    // SAFETY: `user` is the `State` this tap was made with, alive until the tap is gone.
    let st = unsafe { &*(user as *const State) };
    // SAFETY: `e` is the event the system hands this callback, valid during it.
    let field = |f| unsafe { CGEventGetIntegerValueField(e, f) };
    if kind == DISABLED_BY_TIMEOUT || kind == DISABLED_BY_USER {
        // Too slow once (or turned off): input back here, and the tap on again (docs 7.4).
        st.end(None, if kind == DISABLED_BY_TIMEOUT { Why::TAP_TIMEOUT } else { Why::TAP_USER });
        // SAFETY: the tap this callback belongs to.
        unsafe { CGEventTapEnable(st.os.tap.load(Ordering::Relaxed), true) };
        return e;
    }
    // What Aun itself posted (a peer driving this Mac): never a crossing, never swallowed.
    if field(SOURCE_USER_DATA) == kvm::MARK {
        return e;
    }
    if !st.crossed.load(Ordering::SeqCst) {
        // Only a plain move starts one: a button or modifier down here would never come up here.
        // SAFETY: as above.
        if kind != MOVED || unsafe { CGEventGetFlags(e) } & MODIFIERS != 0 || !st.peer.load(Ordering::Relaxed) {
            return e;
        }
        // SAFETY: as above.
        let p = unsafe { CGEventGetLocation(e) };
        let (dx, dy) = (field(DELTA_X) as f64, field(DELTA_Y) as f64);
        if !st.armed.load(Ordering::Relaxed) {
            if st.edge_distance(st.bounds(), (p.x, p.y)) >= REARM {
                st.armed.store(true, Ordering::Relaxed);
            }
            return e;
        }
        if kvm::past(st.bounds(), st.side, (p.x, p.y), dx, dy).is_none() {
            return e;
        }
        // Past the box last read: read it again (a display changed) before going.
        let Some(at) = kvm::past(st.reread(), st.side, (p.x, p.y), dx, dy) else { return e };
        // Not queued, it never started: the sender counts crossings by their Enter.
        if st.out.try_send(Captured::Enter(at)).is_err() {
            return e;
        }
        st.at.store(at.into(), Ordering::Relaxed);
        st.lock_at((p.x, p.y));
        st.crossed.store(true, Ordering::SeqCst);
        // SAFETY: a plain call. The pointer stays where it is; the deltas still come.
        unsafe { CGAssociateMouseAndMouseCursorPosition(false) };
        return std::ptr::null_mut();
    }
    // SAFETY: as above.
    let flags = unsafe { CGEventGetFlags(e) };
    let c = match kind {
        MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED => {
            // Detached above, the cursor still follows the mouse unless Aun is the front app (seen
            // 2026-11-22 from a background process): put it back each time. Measured then: the
            // deltas keep coming at the same rate and don't take the warp in.
            let (x, y) = st.locked();
            // SAFETY: a plain call taking a value.
            unsafe { CGWarpMouseCursorPosition(Point { x, y }) };
            Captured::Move(field(DELTA_X) as i32, field(DELTA_Y) as i32)
        }
        LEFT_DOWN | LEFT_UP => Captured::Button(0, kind == LEFT_DOWN, field(CLICK_STATE).clamp(1, 255) as u8),
        RIGHT_DOWN | RIGHT_UP => Captured::Button(1, kind == RIGHT_DOWN, field(CLICK_STATE).clamp(1, 255) as u8),
        OTHER_DOWN | OTHER_UP if field(BUTTON_NUMBER) == 2 => Captured::Button(2, kind == OTHER_DOWN, field(CLICK_STATE).clamp(1, 255) as u8),
        SCROLL => Captured::Wheel(field(SCROLL_POINT_X) as i32, field(SCROLL_POINT_Y) as i32),
        KEY_DOWN | KEY_UP => {
            let Some(hid) = kvm::hid_of(field(KEYCODE) as u32) else { return std::ptr::null_mut() };
            if kind == KEY_DOWN && kvm::escape(hid, mods(flags)) {
                st.end(None, Why::ESCAPE);
                return std::ptr::null_mut();
            }
            // The input source shortcut (this Mac: Caps Lock made F18) is 한/영 over there, sent
            // on the way down; its up goes nowhere. Swallowed, this Mac's own source stays.
            if kvm::is_input_source_key(hid, flags, &st.os.lang) {
                if kind == KEY_DOWN && st.send(Captured::Key(0x90, true)) {
                    st.send(Captured::Key(0x90, false));
                }
                return std::ptr::null_mut();
            }
            Captured::Key(hid, kind == KEY_DOWN)
        }
        FLAGS_CHANGED => {
            let code = field(KEYCODE) as u16;
            if code == CAPS_LOCK {
                // Comes once per press, as the lock toggles: a press there.
                if st.send(Captured::Key(0x39, true)) {
                    st.send(Captured::Key(0x39, false));
                }
                return std::ptr::null_mut();
            }
            let Some((hid, down)) = kvm::mac_modifier(code, flags) else { return std::ptr::null_mut() };
            Captured::Key(hid, down)
        }
        _ => return std::ptr::null_mut(),
    };
    st.send(c);
    std::ptr::null_mut()
}

/// Asks for Input Monitoring if need be, and puts the tap on its own thread's run loop. Ok with
/// that run loop, for [`stop`].
pub(super) fn start(st: &Arc<State>) -> Result<usize, String> {
    // SAFETY: plain calls.
    let listen = unsafe { CGPreflightListenEventAccess() };
    crate::log(format_args!("cross: Input Monitoring granted: {listen}, Accessibility: {}", kvm::allowed(false)));
    // SAFETY: a plain call; it opens the system's request the first time.
    if !listen && !unsafe { CGRequestListenEventAccess() } {
        return Err("Input Monitoring not granted".into());
    }
    let (made_tx, made) = mpsc::sync_channel(1);
    let user = Arc::into_raw(st.clone()) as usize;
    std::thread::Builder::new().name("aun-tap".into()).spawn(move || {
        let mask = [LEFT_DOWN, LEFT_UP, RIGHT_DOWN, RIGHT_UP, MOVED, LEFT_DRAGGED, RIGHT_DRAGGED, KEY_DOWN, KEY_UP, FLAGS_CHANGED, SCROLL, OTHER_DOWN, OTHER_UP, OTHER_DRAGGED].iter().fold(0u64, |m, t| m | 1 << t);
        // SAFETY: `user` is a `State` kept alive (the raw Arc) until after this run loop
        // stops and the tap is invalidated; the source and tap are released here.
        unsafe {
            let port = CGEventTapCreate(SESSION_TAP, HEAD_INSERT, ACTIVE, mask, tap, user as *mut c_void);
            if port.is_null() {
                drop(Arc::from_raw(user as *const State));
                let _ = made_tx.send(None);
                return;
            }
            (*(user as *const State)).os.tap.store(port, Ordering::Relaxed);
            let src = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), port, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = made_tx.send(Some(rl as usize));
            CFRunLoopRun();
            CGEventTapEnable(port, false);
            CFMachPortInvalidate(port);
            CFRelease(src);
            CFRelease(port);
            let st = Arc::from_raw(user as *const State);
            st.end(None, Why::STOPPED);
        }
    }).map_err(|e| e.to_string())?;
    made.recv().ok().flatten().ok_or_else(|| "event tap not made (a permission missing?)".into())
}

pub(super) fn stop(st: &State, rl: usize) {
    // SAFETY: the tap (alive until its thread invalidates it after the run loop stops), then
    // that thread's run loop. Off first, so a stop that lands before the loop runs leaves no
    // live tap.
    unsafe {
        CGEventTapEnable(st.os.tap.load(Ordering::Relaxed), false);
        CFRunLoopStop(rl as Ref);
    }
}

/// Puts the cursor back with the mouse, for a panic or a quit while across: whether macOS does
/// it when the process ends is not known (docs 7.5).
pub fn release_cursor() {
    // SAFETY: a plain call.
    unsafe { CGAssociateMouseAndMouseCursorPosition(true) };
}
