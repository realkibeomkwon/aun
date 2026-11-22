//! Mouse and keyboard input from a paired device, injected here (docs/kvm-design.md, step 1).
//! Nothing is captured: the driver is a pad in the other device's Aun window.
//!
//! What matters most is that nothing stays held: a button or key the driver pressed is released
//! when it lets go, when the link drops, and when this device stops allowing remote control.

use winit::keyboard::KeyCode;

use crate::wire::Frame;

/// Keys by where they are: winit's `KeyCode` (W3C `code`) and the USB HID usage that goes on the
/// wire. Each OS's own code for a key comes from winit, so this is the only table.
const KEYS: &[(KeyCode, u16)] = {
    use KeyCode as K;
    &[
    (K::KeyA, 0x04), (K::KeyB, 0x05), (K::KeyC, 0x06), (K::KeyD, 0x07), (K::KeyE, 0x08),
    (K::KeyF, 0x09), (K::KeyG, 0x0A), (K::KeyH, 0x0B), (K::KeyI, 0x0C), (K::KeyJ, 0x0D),
    (K::KeyK, 0x0E), (K::KeyL, 0x0F), (K::KeyM, 0x10), (K::KeyN, 0x11), (K::KeyO, 0x12),
    (K::KeyP, 0x13), (K::KeyQ, 0x14), (K::KeyR, 0x15), (K::KeyS, 0x16), (K::KeyT, 0x17),
    (K::KeyU, 0x18), (K::KeyV, 0x19), (K::KeyW, 0x1A), (K::KeyX, 0x1B), (K::KeyY, 0x1C),
    (K::KeyZ, 0x1D), (K::Digit1, 0x1E), (K::Digit2, 0x1F), (K::Digit3, 0x20), (K::Digit4, 0x21),
    (K::Digit5, 0x22), (K::Digit6, 0x23), (K::Digit7, 0x24), (K::Digit8, 0x25), (K::Digit9, 0x26),
    (K::Digit0, 0x27), (K::Enter, 0x28), (K::Escape, 0x29), (K::Backspace, 0x2A), (K::Tab, 0x2B),
    (K::Space, 0x2C), (K::Minus, 0x2D), (K::Equal, 0x2E), (K::BracketLeft, 0x2F),
    (K::BracketRight, 0x30), (K::Backslash, 0x31), (K::Semicolon, 0x33), (K::Quote, 0x34),
    (K::Backquote, 0x35), (K::Comma, 0x36), (K::Period, 0x37), (K::Slash, 0x38),
    (K::CapsLock, 0x39), (K::F1, 0x3A), (K::F2, 0x3B), (K::F3, 0x3C), (K::F4, 0x3D), (K::F5, 0x3E),
    (K::F6, 0x3F), (K::F7, 0x40), (K::F8, 0x41), (K::F9, 0x42), (K::F10, 0x43), (K::F11, 0x44),
    (K::F12, 0x45), (K::PrintScreen, 0x46), (K::ScrollLock, 0x47), (K::Pause, 0x48),
    (K::Insert, 0x49), (K::Home, 0x4A), (K::PageUp, 0x4B), (K::Delete, 0x4C), (K::End, 0x4D),
    (K::PageDown, 0x4E), (K::ArrowRight, 0x4F), (K::ArrowLeft, 0x50), (K::ArrowDown, 0x51),
    (K::ArrowUp, 0x52), (K::NumLock, 0x53), (K::NumpadDivide, 0x54), (K::NumpadMultiply, 0x55),
    (K::NumpadSubtract, 0x56), (K::NumpadAdd, 0x57), (K::NumpadEnter, 0x58), (K::Numpad1, 0x59),
    (K::Numpad2, 0x5A), (K::Numpad3, 0x5B), (K::Numpad4, 0x5C), (K::Numpad5, 0x5D),
    (K::Numpad6, 0x5E), (K::Numpad7, 0x5F), (K::Numpad8, 0x60), (K::Numpad9, 0x61),
    (K::Numpad0, 0x62), (K::NumpadDecimal, 0x63), (K::IntlBackslash, 0x64), (K::ContextMenu, 0x65),
    (K::NumpadEqual, 0x67), (K::F13, 0x68), (K::F14, 0x69), (K::F15, 0x6A), (K::F16, 0x6B),
    (K::F17, 0x6C), (K::F18, 0x6D), (K::F19, 0x6E), (K::F20, 0x6F), (K::F21, 0x70), (K::F22, 0x71),
    (K::F23, 0x72), (K::F24, 0x73), (K::Help, 0x75), (K::AudioVolumeMute, 0x7F),
    (K::AudioVolumeUp, 0x80), (K::AudioVolumeDown, 0x81), (K::NumpadComma, 0x85), (K::IntlRo, 0x87),
    (K::KanaMode, 0x88), (K::IntlYen, 0x89), (K::Convert, 0x8A), (K::NonConvert, 0x8B),
    (K::Lang1, 0x90), (K::Lang2, 0x91), (K::Lang3, 0x92), (K::Lang4, 0x93), (K::Lang5, 0x94),
    (K::ControlLeft, 0xE0), (K::ShiftLeft, 0xE1), (K::AltLeft, 0xE2), (K::SuperLeft, 0xE3),
    (K::ControlRight, 0xE4), (K::ShiftRight, 0xE5), (K::AltRight, 0xE6), (K::SuperRight, 0xE7),
    ]
};

/// The wire code for a key, if it is one remote control sends.
pub fn hid(key: KeyCode) -> Option<u16> {
    KEYS.iter().find(|k| k.0 == key).map(|k| k.1)
}

/// This OS's code for a wire key: a macOS virtual key code, or a Windows scan code (`0xE0xx`
/// extended).
#[cfg(any(target_os = "macos", windows))]
fn scancode(hid: u16) -> Option<u32> {
    use winit::platform::scancode::PhysicalKeyExtScancode;
    let key = KEYS.iter().find(|k| k.1 == hid)?.0;
    winit::keyboard::PhysicalKey::Code(key).to_scancode().or_else(|| MISSING.iter().find(|m| m.0 == key).map(|m| m.1))
}

/// Keys this OS has that winit's table leaves out (its reverse table has some of them).
#[cfg(target_os = "macos")]
const MISSING: &[(KeyCode, u32)] = &[(KeyCode::CapsLock, 0x39), (KeyCode::IntlBackslash, 0x0A), (KeyCode::IntlRo, 0x5E), (KeyCode::NumpadComma, 0x5F), (KeyCode::AudioVolumeMute, 0x4A)];
#[cfg(windows)]
const MISSING: &[(KeyCode, u32)] = &[];

/// Where input lands: the OS, or a recorder in tests.
pub trait Sink {
    /// Moves the pointer by pixels; `held` has bit `b` set for each button `b` down.
    fn move_by(&mut self, dx: i32, dy: i32, held: u8);
    fn button(&mut self, button: u8, down: bool, clicks: u8);
    fn wheel(&mut self, dx: i32, dy: i32);
    /// `hid`: see [`hid`]. A down while held is a repeat.
    fn key(&mut self, hid: u16, down: bool);
}

impl<S: Sink + ?Sized> Sink for Box<S> {
    fn move_by(&mut self, dx: i32, dy: i32, held: u8) {
        (**self).move_by(dx, dy, held)
    }
    fn button(&mut self, button: u8, down: bool, clicks: u8) {
        (**self).button(button, down, clicks)
    }
    fn wheel(&mut self, dx: i32, dy: i32) {
        (**self).wheel(dx, dy)
    }
    fn key(&mut self, hid: u16, down: bool) {
        (**self).key(hid, down)
    }
}

/// One link's input: the moves applied so far and the buttons and keys it holds down.
pub struct Injector<S: Sink> {
    sink: S,
    /// The route, move number and total of the last move applied.
    last: Option<(u32, u32, i32, i32)>,
    held: u8,
    /// In the order pressed.
    keys: Vec<u16>,
}

impl<S: Sink> Injector<S> {
    pub fn new(sink: S) -> Self {
        Injector { sink, last: None, held: 0, keys: Vec::new() }
    }

    pub fn apply(&mut self, f: Frame) {
        match f {
            Frame::KvmMove { route, mn, cx, cy } => {
                let from = match self.last {
                    Some((r, m, _, _)) if r == route && mn <= m => return,
                    Some((r, _, px, py)) if r == route => Some((px, py)),
                    // Joined midway (a replaced link, control allowed late): a baseline only.
                    _ => (mn == 1).then_some((0, 0)),
                };
                self.last = Some((route, mn, cx, cy));
                let Some((px, py)) = from else { return };
                let (dx, dy) = (cx.wrapping_sub(px), cy.wrapping_sub(py));
                if dx != 0 || dy != 0 {
                    self.sink.move_by(dx, dy, self.held);
                }
            }
            Frame::KvmButton { button, down, clicks } if button < 3 => {
                let bit = 1 << button;
                // An up for a button not held (or a second down) changes nothing on the OS's side
                // either way; sending it could end a press made at this device's own mouse.
                if down == (self.held & bit != 0) {
                    return;
                }
                self.held ^= bit;
                self.sink.button(button, down, clicks.max(1));
            }
            Frame::KvmWheel { dx, dy } => self.sink.wheel(dx.into(), dy.into()),
            Frame::KvmLeave => self.release(),
            Frame::KvmKey { hid, down } => {
                let at = self.keys.iter().position(|&k| k == hid);
                match (down, at) {
                    (true, None) => self.keys.push(hid),
                    (true, Some(_)) => {} // a repeat: the OS doesn't repeat injected keys itself
                    // Not held here: an up could end a press at this device's own keyboard.
                    (false, None) => return,
                    (false, Some(i)) => drop(self.keys.remove(i)),
                }
                self.sink.key(hid, down);
            }
            _ => {}
        }
    }

    /// Lets go of every key and button held down here, keys last pressed first.
    pub fn release(&mut self) {
        while let Some(k) = self.keys.pop() {
            self.sink.key(k, false);
        }
        for b in 0..3 {
            if self.held & (1 << b) != 0 {
                self.sink.button(b, false, 1);
            }
        }
        self.held = 0;
        self.last = None;
    }
}

impl<S: Sink> Drop for Injector<S> {
    fn drop(&mut self) {
        self.release();
    }
}

/// The OS's input queue.
#[derive(Default)]
pub struct Os {
    /// macOS: the modifier keys held (a bit per `mac::MODS` entry). Posted events carry their
    /// modifiers as flags, set on each one: a Shift-click is a click with the Shift flag.
    #[cfg(target_os = "macos")]
    mods: u8,
}

/// Whether this process may inject input. macOS asks for Accessibility (and, with `prompt`,
/// opens the request); events posted without it are dropped silently. Windows needs nothing.
pub fn allowed(prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    return mac::trusted(prompt);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        true
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{Os, Sink};
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Point,
    }
    type Ref = *mut c_void;

    const HID_TAP: u32 = 0;
    const CLICK_STATE: u32 = 1;
    const DELTA_X: u32 = 4;
    const DELTA_Y: u32 = 5;
    const PIXEL: u32 = 0;
    // kCGEventLeftMouseDown, RightMouseDown, OtherMouseDown; up is one more.
    const DOWN: [u32; 3] = [1, 3, 25];
    // kCGEventMouseMoved, then dragged with the left, right, other button.
    const MOVED: u32 = 5;
    const DRAGGED: [u32; 3] = [6, 7, 27];
    const FLAGS_CHANGED: u32 = 12;
    /// Modifier key codes (`kVK_*`) and their `kCGEventFlagMask*`: Shift, Control, Option, Command.
    const MODS: [(u32, u64); 8] = [(0x38, 0x2_0000), (0x3C, 0x2_0000), (0x3B, 0x4_0000), (0x3E, 0x4_0000), (0x3A, 0x8_0000), (0x3D, 0x8_0000), (0x37, 0x10_0000), (0x36, 0x10_0000)];

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: Ref) -> Ref;
        fn CGEventGetLocation(e: Ref) -> Point;
        fn CGEventCreateMouseEvent(source: Ref, kind: u32, at: Point, button: u32) -> Ref;
        fn CGEventCreateScrollWheelEvent2(source: Ref, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> Ref;
        fn CGEventSetIntegerValueField(e: Ref, field: u32, v: i64);
        fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
        fn CGEventSetFlags(e: Ref, flags: u64);
        fn CGEventSetType(e: Ref, kind: u32);
        fn CGEventPost(tap: u32, e: Ref);
        fn CGGetActiveDisplayList(max: u32, out: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayBounds(d: u32) -> Rect;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(r: Ref);
        fn CFDictionaryCreate(alloc: Ref, keys: *const Ref, values: *const Ref, n: isize, kcb: *const c_void, vcb: *const c_void) -> Ref;
        static kCFBooleanTrue: Ref;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: Ref) -> bool;
        static kAXTrustedCheckOptionPrompt: Ref;
    }

    pub fn trusted(prompt: bool) -> bool {
        // SAFETY: a one-entry dictionary of CF constants, released after the call that reads it.
        unsafe {
            if !prompt {
                return AXIsProcessTrusted();
            }
            let (k, v) = (kAXTrustedCheckOptionPrompt, kCFBooleanTrue);
            let opts = CFDictionaryCreate(std::ptr::null_mut(), &k, &v, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            let ok = AXIsProcessTrustedWithOptions(opts);
            if !opts.is_null() {
                CFRelease(opts);
            }
            ok
        }
    }

    /// Where the pointer is now, in global display points (origin top-left of the main display).
    fn location() -> Point {
        // SAFETY: an event made only to read the location, released at once.
        unsafe {
            let e = CGEventCreate(std::ptr::null_mut());
            if e.is_null() {
                return Point { x: 0.0, y: 0.0 };
            }
            let p = CGEventGetLocation(e);
            CFRelease(e);
            p
        }
    }

    /// The bounding box of every active display.
    /// shortcut: a box, so a point in the gap beside a shorter display passes; the window server
    /// keeps the pointer on a screen anyway. Use the screens themselves if that shows up.
    fn clamp(p: Point) -> Point {
        let mut ids = [0u32; 16];
        let mut n = 0;
        // SAFETY: `ids` holds 16 entries, the most asked for.
        if unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) } != 0 || n == 0 {
            return p;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &d in &ids[..n as usize] {
            // SAFETY: `d` came from the display list.
            let r = unsafe { CGDisplayBounds(d) };
            (x0, y0) = (x0.min(r.origin.x), y0.min(r.origin.y));
            (x1, y1) = (x1.max(r.origin.x + r.size.x - 1.0), y1.max(r.origin.y + r.size.y - 1.0));
        }
        Point { x: p.x.clamp(x0, x1), y: p.y.clamp(y0, y1) }
    }

    impl Os {
        fn flags(&self) -> u64 {
            MODS.iter().enumerate().filter(|(i, _)| self.mods & (1 << i) != 0).fold(0, |f, (_, m)| f | m.1)
        }

        /// Posts `e` with the modifiers held (none held: the event's own flags stay).
        fn post(&self, e: Ref) {
            if e.is_null() {
                return;
            }
            // SAFETY: `e` is an event this module created; posting doesn't take ownership.
            unsafe {
                let flags = self.flags();
                if flags != 0 {
                    CGEventSetFlags(e, flags);
                }
                CGEventPost(HID_TAP, e);
                CFRelease(e);
            }
        }
    }

    impl Sink for Os {
        fn move_by(&mut self, dx: i32, dy: i32, held: u8) {
            let p = location();
            let to = clamp(Point { x: p.x + dx as f64, y: p.y + dy as f64 });
            // Held, it is a drag of the lowest button held: apps track drags by that event type.
            let (kind, button) = match (0..3).find(|b| held & (1 << b) != 0) {
                Some(b) => (DRAGGED[b], b as u32),
                None => (MOVED, 0),
            };
            // SAFETY: a new event, posted and released by `post`.
            unsafe {
                let e = CGEventCreateMouseEvent(std::ptr::null_mut(), kind, to, button);
                if !e.is_null() {
                    // Games and 3D views read only the deltas.
                    CGEventSetIntegerValueField(e, DELTA_X, dx.into());
                    CGEventSetIntegerValueField(e, DELTA_Y, dy.into());
                }
                self.post(e);
            }
        }

        fn button(&mut self, button: u8, down: bool, clicks: u8) {
            let kind = DOWN[button as usize] + !down as u32;
            // SAFETY: as above.
            unsafe {
                let e = CGEventCreateMouseEvent(std::ptr::null_mut(), kind, location(), button.into());
                if !e.is_null() {
                    CGEventSetIntegerValueField(e, CLICK_STATE, clicks.into());
                }
                self.post(e);
            }
        }

        fn wheel(&mut self, dx: i32, dy: i32) {
            // SAFETY: as above.
            self.post(unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null_mut(), PIXEL, 2, dy, dx, 0) });
        }

        fn key(&mut self, hid: u16, down: bool) {
            let Some(code) = super::scancode(hid) else { return };
            let m = MODS.iter().position(|m| m.0 == code);
            if let Some(i) = m {
                self.mods = if down { self.mods | 1 << i } else { self.mods & !(1 << i) };
            }
            // SAFETY: a new event, posted and released by `post`.
            unsafe {
                let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code as u16, down);
                if e.is_null() {
                    return;
                }
                if m.is_some() {
                    // Apps see a modifier as flags changing, not as a key going down.
                    CGEventSetType(e, FLAGS_CHANGED);
                    CGEventSetFlags(e, self.flags());
                }
                self.post(e);
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use super::{Os, Sink};
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    /// In `dwExtraInfo` of what Aun injects, so a later capture hook can tell it apart.
    pub const MARK: usize = 0x4155_4E31; // "AUN1"
    /// Pixels of scrolling per wheel notch (`WHEEL_DELTA`, 120).
    /// shortcut: a guess at what a notch scrolls; tune on the device.
    const PX_PER_NOTCH: i32 = 40;

    fn send(flags: u32, dx: i32, dy: i32, data: i32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: MARK } },
        };
        // SAFETY: one initialised INPUT of the size given.
        unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    }

    impl Sink for Os {
        fn move_by(&mut self, dx: i32, dy: i32, _held: u8) {
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: plain reads of the cursor and the virtual screen.
            let (x0, y0, w, h) = unsafe {
                GetCursorPos(&mut p);
                (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2), GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2))
            };
            // Absolute, so the system's pointer acceleration isn't applied a second time.
            let x = p.x.saturating_add(dx).clamp(x0, x0 + w - 1);
            let y = p.y.saturating_add(dy).clamp(y0, y0 + h - 1);
            let nx = ((x - x0) as i64 * 65535 / (w - 1) as i64) as i32;
            let ny = ((y - y0) as i64 * 65535 / (h - 1) as i64) as i32;
            send(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0);
        }

        fn button(&mut self, button: u8, down: bool, _clicks: u8) {
            // Windows counts double clicks itself, from timing and distance.
            let flag = match (button, down) {
                (0, true) => MOUSEEVENTF_LEFTDOWN,
                (0, false) => MOUSEEVENTF_LEFTUP,
                (1, true) => MOUSEEVENTF_RIGHTDOWN,
                (1, false) => MOUSEEVENTF_RIGHTUP,
                (_, true) => MOUSEEVENTF_MIDDLEDOWN,
                (_, false) => MOUSEEVENTF_MIDDLEUP,
            };
            send(flag, 0, 0, 0);
        }

        fn wheel(&mut self, dx: i32, dy: i32) {
            // Positive WHEEL scrolls up (content down); positive HWHEEL scrolls right (content left).
            if dy != 0 {
                send(MOUSEEVENTF_WHEEL, 0, 0, dy * WHEEL_DELTA as i32 / PX_PER_NOTCH);
            }
            if dx != 0 {
                send(MOUSEEVENTF_HWHEEL, 0, 0, -dx * WHEEL_DELTA as i32 / PX_PER_NOTCH);
            }
        }

        fn key(&mut self, hid: u16, down: bool) {
            let Some(sc) = super::scancode(hid) else { return };
            // By scan code: a virtual key would type what the key means in this PC's layout.
            let mut flags = KEYEVENTF_SCANCODE;
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            if sc & 0xFF00 == 0xE000 {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: (sc & 0xFF) as u16, dwFlags: flags, time: 0, dwExtraInfo: MARK } },
            };
            // SAFETY: one initialised INPUT of the size given.
            unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
impl Sink for Os {
    fn move_by(&mut self, _: i32, _: i32, _: u8) {}
    fn button(&mut self, _: u8, _: bool, _: u8) {}
    fn wheel(&mut self, _: i32, _: i32) {}
    fn key(&mut self, _: u16, _: bool) {}
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Rec(pub Arc<Mutex<Vec<String>>>);

    impl Sink for Rec {
        fn move_by(&mut self, dx: i32, dy: i32, held: u8) {
            self.0.lock().unwrap().push(format!("move {dx} {dy} held {held}"));
        }
        fn button(&mut self, b: u8, down: bool, clicks: u8) {
            self.0.lock().unwrap().push(format!("button {b} {} x{clicks}", if down { "down" } else { "up" }));
        }
        fn wheel(&mut self, dx: i32, dy: i32) {
            self.0.lock().unwrap().push(format!("wheel {dx} {dy}"));
        }
        fn key(&mut self, hid: u16, down: bool) {
            self.0.lock().unwrap().push(format!("key {hid:#x} {}", if down { "down" } else { "up" }));
        }
    }

    /// Every key has one wire code and back, and this OS injects every key it has: one the
    /// table names that the OS has but can't be given would be dropped at the receiver unnoticed.
    #[test]
    fn every_key_round_trips_and_this_os_can_inject_the_ones_it_has() {
        use KeyCode as K;
        // Keys with no place on this OS's keyboards; anything else must have a code.
        #[cfg(target_os = "macos")]
        let absent = [K::ContextMenu, K::Convert, K::F21, K::F22, K::F23, K::F24, K::Help, K::KanaMode, K::Lang1, K::Lang2, K::Lang3, K::Lang4, K::Lang5, K::NonConvert, K::Pause, K::PrintScreen, K::ScrollLock];
        #[cfg(windows)]
        let absent = [K::Help, K::Lang3, K::Lang4, K::Lang5];
        for &(k, h) in KEYS {
            assert_eq!(hid(k), Some(h), "{k:?}");
            assert_eq!(KEYS.iter().filter(|e| e.1 == h).count(), 1, "{h:#x} is on one key only");
            #[cfg(any(target_os = "macos", windows))]
            assert_eq!(scancode(h).is_some(), !absent.contains(&k), "{k:?}: a code on this OS");
        }
    }

    #[test]
    fn keys_repeat_go_up_once_and_are_released_last_pressed_first() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        let key = |hid, down| Frame::KvmKey { hid, down };
        inj.apply(key(0xE1, true)); // Shift
        inj.apply(key(0x04, true)); // A
        inj.apply(key(0x04, true)); // A repeating
        inj.apply(key(0x04, false));
        inj.apply(key(0x04, false)); // not held any more
        inj.apply(key(0x05, true));
        inj.apply(Frame::KvmButton { button: 0, down: true, clicks: 1 });
        inj.apply(Frame::KvmLeave);
        inj.apply(key(0xE0, true));
        drop(inj);
        assert_eq!(*rec.0.lock().unwrap(), [
            "key 0xe1 down", "key 0x4 down", "key 0x4 down", "key 0x4 up", "key 0x5 down", "button 0 down x1",
            "key 0x5 up", "key 0xe1 up", "button 0 up x1", "key 0xe0 down", "key 0xe0 up",
        ]);
    }

    fn mv(route: u32, mn: u32, cx: i32, cy: i32) -> Frame {
        Frame::KvmMove { route, mn, cx, cy }
    }

    #[test]
    fn moves_apply_once_each_in_order_and_a_lost_one_loses_no_distance() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        inj.apply(mv(1, 1, 10, 0));
        inj.apply(mv(1, 1, 10, 0)); // again
        inj.apply(mv(1, 3, 25, -5)); // 2 lost
        inj.apply(mv(1, 2, 18, -2)); // 2 late: older than 3
        inj.apply(mv(2, 1, 4, 4)); // a new route starts from nothing
        assert_eq!(*rec.0.lock().unwrap(), ["move 10 0 held 0", "move 15 -5 held 0", "move 4 4 held 0"]);
    }

    /// A link that replaced another, or control allowed with the pad already open, first sees a
    /// move well into a route: it is where the pointer is now, not a distance to go.
    #[test]
    fn a_route_joined_midway_starts_from_its_next_move() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        inj.apply(mv(1, 40, 800, 300));
        inj.apply(mv(1, 41, 806, 302));
        inj.apply(Frame::KvmLeave);
        inj.apply(mv(1, 42, 900, 302));
        inj.apply(mv(1, 43, 901, 302));
        assert_eq!(*rec.0.lock().unwrap(), ["move 6 2 held 0", "move 1 0 held 0"]);
    }

    #[test]
    fn buttons_held_are_released_on_leave_and_on_drop_and_only_those() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        inj.apply(Frame::KvmButton { button: 0, down: true, clicks: 1 });
        inj.apply(Frame::KvmButton { button: 0, down: true, clicks: 1 }); // no second down
        inj.apply(mv(1, 1, 3, 0));
        inj.apply(Frame::KvmButton { button: 1, down: false, clicks: 1 }); // never held: not sent
        inj.apply(Frame::KvmLeave);
        inj.apply(Frame::KvmButton { button: 2, down: true, clicks: 0 });
        drop(inj);
        assert_eq!(*rec.0.lock().unwrap(), ["button 0 down x1", "move 3 0 held 1", "button 0 up x1", "button 2 down x1", "button 2 up x1"]);
    }
}
