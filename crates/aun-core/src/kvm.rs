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
    MISSING.iter().find(|m| m.0 == key).map(|m| m.1).or_else(|| winit::keyboard::PhysicalKey::Code(key).to_scancode())
}

/// The wire key for this OS's own code for it ([`scancode`] backwards).
#[cfg(any(target_os = "macos", windows))]
pub fn hid_of(code: u32) -> Option<u16> {
    KEYS.iter().map(|k| k.1).find(|&h| scancode(h) == Some(code))
}

/// Ctrl, Alt (Option), Shift and Esc, either side: the way home from a crossing, never sent on
/// (docs/kvm-design.md 7.3). `mods` has bit `i` set for HID modifier `0xE0 + i` held.
pub fn escape(hid: u16, mods: u8) -> bool {
    hid == 0x29 && mods & 0x11 != 0 && mods & 0x44 != 0 && mods & 0x22 != 0
}

/// macOS: the modifier key a `flagsChanged` event with key code `code` is about, as a wire key,
/// and whether it went down, from the event's per-side flag bits (`NX_DEVICE*KEYMASK`): with
/// both Shifts held, the Shift flag alone can't say which went up.
pub fn mac_modifier(code: u16, flags: u64) -> Option<(u16, bool)> {
    // kVK_*, the HID usage, the device bit.
    const MODS: [(u16, u16, u64); 8] = [(0x3B, 0xE0, 0x1), (0x38, 0xE1, 0x2), (0x3A, 0xE2, 0x20), (0x37, 0xE3, 0x8), (0x3E, 0xE4, 0x2000), (0x3C, 0xE5, 0x4), (0x3D, 0xE6, 0x40), (0x36, 0xE7, 0x10)];
    MODS.iter().find(|m| m.0 == code).map(|m| (m.1, flags & m.2 != 0))
}

/// On what Aun injects ("AUN1"), so its own capture can tell it apart: Windows `dwExtraInfo`,
/// macOS `kCGEventSourceUserData`.
#[cfg(target_os = "macos")]
pub(crate) const MARK: i64 = 0x4155_4E31;

/// Control and ⌘ (the Windows key) swapped, left and right, when `swap` (the app's setting).
pub fn swapped(hid: u16, swap: bool) -> u16 {
    match hid {
        0xE0 | 0xE4 if swap => hid + 3,
        0xE3 | 0xE7 if swap => hid - 3,
        _ => hid,
    }
}

/// macOS: the box around every display, in global display points.
#[cfg(target_os = "macos")]
pub(crate) fn screen_bounds() -> Option<Bounds> {
    mac::bounds()
}

/// What a capture saw, in wire terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Captured {
    /// The pointer went out past the edge toward the peer, `at` along it (see [`past`]).
    Enter(u16),
    Move(i32, i32),
    Button(u8, bool, u8),
    Wheel(i32, i32),
    Key(u16, bool),
    /// The crossing ended here (escape key, a stall, the queue full): let go over there.
    Leave,
}

/// The sending side of crossings: numbers each one and keeps its running move totals.
#[derive(Default)]
pub struct Driver {
    route: u32,
    mn: u32,
    total: (i32, i32),
}

impl Driver {
    /// `side`: where the peer is from here, as in `KvmEnter`; it is entered by the opposite edge.
    pub fn frame(&mut self, c: Captured, side: u8) -> Frame {
        match c {
            Captured::Enter(at) => {
                (self.route, self.mn, self.total) = (self.route.wrapping_add(1), 0, (0, 0));
                Frame::KvmEnter { route: self.route, side: side ^ 1, at }
            }
            Captured::Move(dx, dy) => {
                self.mn += 1;
                self.total = (self.total.0.wrapping_add(dx), self.total.1.wrapping_add(dy));
                Frame::KvmMove { route: self.route, mn: self.mn, cx: self.total.0, cy: self.total.1 }
            }
            Captured::Button(button, down, clicks) => Frame::KvmButton { button, down, clicks },
            Captured::Wheel(dx, dy) => Frame::KvmWheel { dx: dx.clamp(-32768, 32767) as i16, dy: dy.clamp(-32768, 32767) as i16 },
            Captured::Key(hid, down) => Frame::KvmKey { hid, down },
            Captured::Leave => Frame::KvmLeave,
        }
    }
}

/// Keys this OS has that winit's table leaves out (its reverse table has some of them) or gets
/// wrong: it has the volume keys one up from Apple's `Events.h` (`kVK_VolumeUp` 0x48,
/// `kVK_VolumeDown` 0x49, `kVK_Mute` 0x4A), so volume down went as mute.
#[cfg(target_os = "macos")]
const MISSING: &[(KeyCode, u32)] = &[(KeyCode::CapsLock, 0x39), (KeyCode::IntlBackslash, 0x0A), (KeyCode::IntlRo, 0x5E), (KeyCode::NumpadComma, 0x5F), (KeyCode::AudioVolumeMute, 0x4A), (KeyCode::AudioVolumeUp, 0x48), (KeyCode::AudioVolumeDown, 0x49)];
#[cfg(windows)]
const MISSING: &[(KeyCode, u32)] = &[];

/// Keys on this keyboard that are 한/영 or 한자 here, and the wire code each goes as (HID Lang1,
/// Lang2) instead of its own. Windows: the keys the Korean layout turns into `VK_HANGUL` and
/// `VK_HANJA` (right Alt and right Ctrl on most). Elsewhere none: a Mac's input source key never
/// reaches an app, see [`on_input_source_change`].
pub fn lang_keys() -> Vec<(KeyCode, u16)> {
    #[cfg(windows)]
    return win::lang_keys();
    #[cfg(not(windows))]
    Vec::new()
}

/// macOS: the input source shortcuts that take modifiers (⌃Space by default), as the key and
/// its modifier flags (Shift `0x20000`, Control `0x40000`, Option `0x80000`, ⌘ `0x100000`).
/// Unlike a bare F-key, such a key reaches the app as well as switching the source (Space after
/// Control, checked 2026-11-22). Elsewhere none.
pub fn input_source_combos() -> Vec<(KeyCode, u64)> {
    #[cfg(target_os = "macos")]
    return mac::input_sources()
        .into_iter()
        .flatten()
        .map(|s| (s.0, s.1 & 0x1E_0000))
        .filter(|s| s.1 != 0)
        .filter_map(|s| KEYS.iter().find(|k| scancode(k.1) == Some(s.0.into())).map(|k| (k.0, s.1)))
        .collect();
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

/// macOS: calls `f` on the main thread each time the input source changes, for the life of the
/// process. The key that switches it (Caps Lock remapped to F18, ⌃Space) goes to the system and
/// no app sees it (checked 2026-11-22), so the switch is all a sender can know of 한/영.
#[cfg(target_os = "macos")]
pub fn on_input_source_change(f: impl Fn() + 'static) {
    use block2::RcBlock;
    use objc2_foundation::{NSDistributedNotificationCenter, NSString};
    let center = NSDistributedNotificationCenter::defaultCenter();
    let name = NSString::from_str("com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged");
    let block = RcBlock::new(move |_| f());
    // SAFETY: no object filter, no queue: delivered on the main run loop, where `f` may run.
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block) };
    std::mem::forget(token);
}

/// macOS's "Select the previous input source" and "next" shortcuts from their entries in
/// `com.apple.symbolichotkeys` (enabled, key code, modifier flags): a key code and flags each,
/// None where off. An entry not there is macOS's default, ⌃Space and ⌃⌥Space.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn input_sources(entries: [Option<(bool, i64, i64)>; 2]) -> [Option<(u16, u64)>; 2] {
    const DEFAULT: [(u16, u64); 2] = [(0x31, 0x4_0000), (0x31, 0xC_0000)];
    let mut out = [None; 2];
    for (i, e) in entries.into_iter().enumerate() {
        out[i] = match e {
            None => Some(DEFAULT[i]),
            Some((true, code @ 0..0xFFFF, flags)) => Some((code as u16, flags as u64)),
            Some(_) => None, // off, or no key
        };
    }
    out
}

/// The box around every screen, in pixels: `x0`, `y0` on it, `x1`, `y1` just past it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// `v` from `a` to the last pixel before `b`, as 0..=65535.
fn frac(v: f64, a: f64, b: f64) -> u16 {
    ((v - a) / (b - a - 1.0).max(1.0)).clamp(0.0, 1.0).mul_add(65535.0, 0.5) as u16
}

/// Where a pointer entering by `side`'s edge (as in `KvmEnter`) at `at` along it starts: on
/// the edge's own pixels, so the first push back out leaves again.
pub fn entry(b: Bounds, side: u8, at: u16) -> (f64, f64) {
    let along = |a: f64, z: f64| (a + f64::from(at) / 65535.0 * (z - a - 1.0)).round();
    match side {
        0 => (b.x0, along(b.y0, b.y1)),
        1 => (b.x1 - 1.0, along(b.y0, b.y1)),
        2 => (along(b.x0, b.x1), b.y0),
        _ => (along(b.x0, b.x1), b.y1 - 1.0),
    }
}

/// Where along `side`'s edge a pointer at `p` moving by `(dx, dy)` goes out past it, if it does.
pub fn past(b: Bounds, side: u8, p: (f64, f64), dx: f64, dy: f64) -> Option<u16> {
    let (x, y) = (p.0 + dx, p.1 + dy);
    let out = match side {
        0 => x < b.x0,
        1 => x >= b.x1,
        2 => y < b.y0,
        _ => y >= b.y1,
    };
    out.then(|| if side < 2 { frac(p.1, b.y0, b.y1) } else { frac(p.0, b.x0, b.x1) })
}

/// Where input lands: the OS, or a recorder in tests.
pub trait Sink {
    /// Moves the pointer by pixels; `held` has bit `b` set for each button `b` down. With an
    /// `edge` (a side, as in `KvmEnter`), a move out past it says where along it ([`past`]).
    fn move_by(&mut self, dx: i32, dy: i32, held: u8, edge: Option<u8>) -> Option<u16>;
    /// Puts the pointer where one entering by `side` at `at` starts ([`entry`]).
    fn enter(&mut self, side: u8, at: u16);
    fn button(&mut self, button: u8, down: bool, clicks: u8);
    fn wheel(&mut self, dx: i32, dy: i32);
    /// `hid`: see [`hid`]. A down while held is a repeat.
    fn key(&mut self, hid: u16, down: bool);
    /// Before letting go of a held Windows key (⌘) the driver didn't let go of itself: keeps it
    /// from acting alone as it goes up (Windows opens Start).
    fn mask(&mut self) {}
}

impl<S: Sink + ?Sized> Sink for Box<S> {
    fn move_by(&mut self, dx: i32, dy: i32, held: u8, edge: Option<u8>) -> Option<u16> {
        (**self).move_by(dx, dy, held, edge)
    }
    fn enter(&mut self, side: u8, at: u16) {
        (**self).enter(side, at)
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
    fn mask(&mut self) {
        (**self).mask()
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
    /// The edge the pointer of a crossing came in by (`KvmEnter`): out through it, it goes back.
    edge: Option<u8>,
}

impl<S: Sink> Injector<S> {
    pub fn new(sink: S) -> Self {
        Injector { sink, last: None, held: 0, keys: Vec::new(), edge: None }
    }

    /// Applies one frame from the driver; returns the answer to send it back, if any (a
    /// `KvmReturn` once the pointer goes back out through the edge it came in by).
    pub fn apply(&mut self, f: Frame) -> Option<Frame> {
        match f {
            Frame::KvmEnter { route: _, side, at } => {
                // A move numbered 1 of this route starts from here, as any new route's does.
                self.last = None;
                self.edge = Some(side);
                self.sink.enter(side, at);
            }
            Frame::KvmMove { route, mn, cx, cy } => {
                let from = match self.last {
                    Some((r, m, _, _)) if r == route && mn <= m => return None,
                    Some((r, _, px, py)) if r == route => Some((px, py)),
                    // Joined midway (a replaced link, control allowed late): a baseline only.
                    _ => (mn == 1).then_some((0, 0)),
                };
                self.last = Some((route, mn, cx, cy));
                let (px, py) = from?;
                let (dx, dy) = (cx.wrapping_sub(px), cy.wrapping_sub(py));
                if (dx != 0 || dy != 0)
                    && let Some(at) = self.sink.move_by(dx, dy, self.held, self.edge)
                {
                    self.edge = None; // once: the driver ends the route with KvmLeave
                    return Some(Frame::KvmReturn { route, at });
                }
            }
            Frame::KvmButton { button, down, clicks } if button < 3 => {
                let bit = 1 << button;
                // An up for a button not held (or a second down) changes nothing on the OS's side
                // either way; sending it could end a press made at this device's own mouse.
                if down == (self.held & bit != 0) {
                    return None;
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
                    // 한/영 and 한자 switch on each down (a Mac presses a shortcut for them).
                    (true, Some(_)) if matches!(hid, 0x90 | 0x91) => return None,
                    (true, Some(_)) => {} // a repeat: the OS doesn't repeat injected keys itself
                    // Not held here: an up could end a press at this device's own keyboard.
                    (false, None) => return None,
                    (false, Some(i)) => drop(self.keys.remove(i)),
                }
                self.sink.key(hid, down);
            }
            _ => {}
        }
        None
    }

    /// Lets go of every key and button held down here, keys last pressed first.
    pub fn release(&mut self) {
        // Left across by the escape key with ⌘·Ctrl swapped, Ctrl is held there as Win.
        if self.keys.iter().any(|&k| matches!(k, 0xE3 | 0xE7)) {
            self.sink.mask();
        }
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
        self.edge = None;
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
    use super::{Bounds, Os, Sink};
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
    /// `kCGEventSourceUserData`: carries [`super::MARK`] on what Aun posts, so its own capture
    /// tap lets it by (a pad pushing this Mac's pointer past the edge isn't a crossing).
    const USER_DATA: u32 = 42;

    /// Posts `e` marked as Aun's own.
    /// SAFETY: `e` is a live event.
    unsafe fn post_marked(e: Ref) {
        // SAFETY: as the caller promises.
        unsafe {
            CGEventSetIntegerValueField(e, USER_DATA, super::MARK);
            CGEventPost(HID_TAP, e);
        }
    }
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

    const OPTION: u64 = 0x8_0000;
    const RETURN: u16 = 0x24;
    /// `kCGEventFlagMaskSecondaryFn` and `kCGEventFlagMaskNumericPad`.
    pub const FN: u64 = 0x80_0000;
    pub const NUMPAD: u64 = 0x20_0000;
    /// Key codes that carry each, as a fresh private event source gives them (all of 0..0x80,
    /// measured 2026-11-22).
    const FN_CODES: [u16; 31] = [0x3F, 0x40, 0x47, 0x4F, 0x50, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x67, 0x69, 0x6A, 0x6B, 0x6D, 0x6F, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E];
    const PAD_CODES: [u16; 21] = [0x41, 0x43, 0x45, 0x4B, 0x4C, 0x4E, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C, 0x7B, 0x7C, 0x7D, 0x7E];

    /// The function and keypad flags a key's events have of their own. An event made from the
    /// session takes them from the last one posted too: after an injected F19 (the input source
    /// shortcut), the PC's next `f` came as Globe+F and filled the screen (2026-11-22).
    pub fn own_flags(code: u16) -> u64 {
        (if FN_CODES.contains(&code) { FN } else { 0 }) | (if PAD_CODES.contains(&code) { NUMPAD } else { 0 })
    }

    /// `kVK_Function`, and the flag every event has at rest.
    const FN_KEY: u16 = 0x3F;
    const NON_COALESCED: u64 = 0x100;
    /// HID Lang1 (한/영 on a Korean keyboard) and Lang2 (한자).
    const LANG1: u16 = 0x90;
    const LANG2: u16 = 0x91;

    /// The input source shortcuts as they are set now (see `super::input_sources`).
    pub fn input_sources() -> [Option<(u16, u64)>; 2] {
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUserDefaults};
        fn get(d: &NSDictionary, key: &str) -> Option<Retained<AnyObject>> {
            d.objectForKey(&NSString::from_str(key))
        }
        let all = NSUserDefaults::standardUserDefaults()
            .persistentDomainForName(&NSString::from_str("com.apple.symbolichotkeys"))
            .and_then(|d| d.objectForKey(&NSString::from_str("AppleSymbolicHotKeys")))
            .and_then(|a| a.downcast::<NSDictionary>().ok());
        let entry = |id: &str| -> Option<(bool, i64, i64)> {
            let e = get(all.as_deref()?, id)?.downcast::<NSDictionary>().ok()?;
            let on = get(&e, "enabled").and_then(|v| v.downcast::<NSNumber>().ok()).is_some_and(|v| v.boolValue());
            let params = get(&e, "value").and_then(|v| v.downcast::<NSDictionary>().ok()).and_then(|v| get(&v, "parameters"))?.downcast::<NSArray>().ok()?;
            let n = |i| params.objectAtIndex(i).downcast::<NSNumber>().ok().map(|n| n.integerValue() as i64);
            if params.count() < 3 {
                return Some((false, 0, 0));
            }
            Some((on, n(1)?, n(2)?))
        };
        // 60 and 61: AppKit's ids for the previous and next input source.
        super::input_sources([entry("60"), entry("61")])
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: Ref) -> Ref;
        fn CGEventGetLocation(e: Ref) -> Point;
        fn CGEventCreateMouseEvent(source: Ref, kind: u32, at: Point, button: u32) -> Ref;
        fn CGEventCreateScrollWheelEvent2(source: Ref, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> Ref;
        fn CGEventSetIntegerValueField(e: Ref, field: u32, v: i64);
        fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
        fn CGEventSetFlags(e: Ref, flags: u64);
        fn CGEventGetFlags(e: Ref) -> u64;
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

    /// The bounding box of every active display, in global display points.
    /// shortcut: a box, so a point in the gap beside a shorter display passes; the window server
    /// keeps the pointer on a screen anyway. Use the screens themselves with several displays.
    pub fn bounds() -> Option<Bounds> {
        let mut ids = [0u32; 16];
        let mut n = 0;
        // SAFETY: `ids` holds 16 entries, the most asked for.
        if unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) } != 0 || n == 0 {
            return None;
        }
        let mut b = Bounds { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN };
        for &d in &ids[..n as usize] {
            // SAFETY: `d` came from the display list.
            let r = unsafe { CGDisplayBounds(d) };
            (b.x0, b.y0) = (b.x0.min(r.origin.x), b.y0.min(r.origin.y));
            (b.x1, b.y1) = (b.x1.max(r.origin.x + r.size.x), b.y1.max(r.origin.y + r.size.y));
        }
        Some(b)
    }

    fn clamp(p: Point, b: Option<Bounds>) -> Point {
        let Some(b) = b else { return p };
        Point { x: p.x.clamp(b.x0, b.x1 - 1.0), y: p.y.clamp(b.y0, b.y1 - 1.0) }
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
                    // A key's own function and keypad flags stay: a Shift-arrow is still an arrow.
                    CGEventSetFlags(e, flags | CGEventGetFlags(e) & (FN | NUMPAD));
                }
                post_marked(e);
                let fn_on = CGEventGetFlags(e) & FN != 0;
                CFRelease(e);
                if fn_on {
                    self.settle();
                }
            }
        }

        /// Puts the system's modifier state back to the modifiers held. A posted event with the
        /// function flag (F-keys and arrows carry it) leaves it set, and every event after it
        /// inherits it: an `f` typed next is Globe+F, which fills the screen with the front
        /// window (seen 2026-11-22).
        fn settle(&self) {
            // SAFETY: a new event, posted and released here.
            unsafe {
                let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), FN_KEY, false);
                if !e.is_null() {
                    CGEventSetType(e, FLAGS_CHANGED);
                    CGEventSetFlags(e, self.flags() | NON_COALESCED);
                    post_marked(e);
                    CFRelease(e);
                }
            }
        }
    }

    impl Os {
        fn move_to(&self, to: Point, dx: i32, dy: i32, held: u8) {
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
    }

    impl Sink for Os {
        fn move_by(&mut self, dx: i32, dy: i32, held: u8, edge: Option<u8>) -> Option<u16> {
            let (p, b) = (location(), bounds());
            let out = edge.zip(b).and_then(|(side, b)| super::past(b, side, (p.x, p.y), dx.into(), dy.into()));
            self.move_to(clamp(Point { x: p.x + dx as f64, y: p.y + dy as f64 }, b), dx, dy, held);
            out
        }

        fn enter(&mut self, side: u8, at: u16) {
            let Some(b) = bounds() else { return };
            let (x, y) = super::entry(b, side, at);
            self.move_to(Point { x, y }, 0, 0, 0);
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
            // A Mac keyboard has neither: 한/영 is the input source shortcut, 한자 the Korean
            // input method's ⌥Return. Pressed once on the way down.
            let tap = match hid {
                LANG1 => input_sources().into_iter().flatten().next(),
                LANG2 => Some((RETURN, OPTION)),
                _ => None,
            };
            if let Some((code, flags)) = tap {
                if down {
                    crate::log(format_args!("input: {} from the peer, pressing key {code:#x} flags {flags:#x}", if hid == LANG1 { "한/영" } else { "한자" }));
                }
                for d in [true, false].into_iter().filter(|_| down) {
                    // SAFETY: a new event, posted and released here.
                    unsafe {
                        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, d);
                        if !e.is_null() {
                            // Added to the event's own: an F-key without its function flag
                            // doesn't fire a shortcut set to it (seen with F19, 2026-11-22).
                            CGEventSetFlags(e, CGEventGetFlags(e) & !(FN | NUMPAD) | own_flags(code) | flags);
                            post_marked(e);
                            CFRelease(e);
                        }
                    }
                }
                if down {
                    self.settle(); // the shortcut's own modifiers and function flag
                }
                return;
            }
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
                } else {
                    CGEventSetFlags(e, CGEventGetFlags(e) & !(FN | NUMPAD) | own_flags(code as u16));
                }
                self.post(e);
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use super::{Bounds, KeyCode, Os, Sink};
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    /// In `dwExtraInfo` of what Aun injects, so a later capture hook can tell it apart.
    pub const MARK: usize = 0x4155_4E31; // "AUN1"
    /// A virtual key no layout assigns (`0xE8`), pressed to keep Win or Alt from acting alone.
    const MASK_KEY: u16 = 0xE8;
    /// Pixels of scrolling per wheel notch (`WHEEL_DELTA`, 120).
    /// shortcut: a guess at what a notch scrolls; tune on the device.
    const PX_PER_NOTCH: i32 = 40;

    fn send(flags: u32, dx: i32, dy: i32, data: i32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: MARK } },
        };
        push(&input);
    }

    /// Whether the last `SendInput` failed, to log only the changes.
    static FAILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Injects one input. Diagnostic (2026-11-22 device test, remote control stopped after a
    /// hot corner showed the desktop): logs when injection starts failing, with the error, and
    /// when it works again.
    fn push(input: &INPUT) {
        // SAFETY: one initialised INPUT of the size given.
        let ok = unsafe { SendInput(1, input, size_of::<INPUT>() as i32) } == 1;
        if FAILING.swap(!ok, std::sync::atomic::Ordering::Relaxed) == ok {
            // SAFETY: a plain read of this thread's last error.
            let e = if ok { 0 } else { unsafe { windows_sys::Win32::Foundation::GetLastError() } };
            crate::log(format_args!("input: SendInput {}", if ok { "works again".to_owned() } else { format!("failed (error {e})") }));
        }
    }

    /// See `super::lang_keys`.
    pub fn lang_keys() -> Vec<(KeyCode, u16)> {
        // SAFETY: this thread's layout and a lookup in it.
        let hkl = unsafe { GetKeyboardLayout(0) };
        if hkl as usize & 0x3FF != 0x12 {
            return Vec::new(); // not Korean: VK_KANA, VK_KANJI share these codes
        }
        super::KEYS
            .iter()
            .filter_map(|&(k, hid)| {
                let sc = super::scancode(hid)?;
                // SAFETY: as above.
                let vk = unsafe { MapVirtualKeyExW(sc, MAPVK_VSC_TO_VK_EX, hkl) } as u16;
                let lang = [(VK_HANGUL, 0x90), (VK_HANJA, 0x91)].iter().find(|l| l.0 == vk)?.1;
                (lang != hid).then_some((k, lang))
            })
            .collect()
    }

    /// The virtual screen: the box around every monitor, in pixels.
    pub fn bounds() -> Bounds {
        // SAFETY: plain reads of the virtual screen.
        let (x0, y0, w, h) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2), GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2)) };
        Bounds { x0: x0.into(), y0: y0.into(), x1: (x0 + w).into(), y1: (y0 + h).into() }
    }

    /// Puts the pointer at `(x, y)`, kept on the virtual screen `b`. Absolute, so the system's
    /// pointer acceleration isn't applied a second time.
    fn move_to(b: Bounds, x: f64, y: f64) {
        let (x, y) = (x.clamp(b.x0, b.x1 - 1.0), y.clamp(b.y0, b.y1 - 1.0));
        let nx = ((x - b.x0) * 65535.0 / (b.x1 - b.x0 - 1.0)) as i32;
        let ny = ((y - b.y0) * 65535.0 / (b.y1 - b.y0 - 1.0)) as i32;
        send(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0);
    }

    impl Sink for Os {
        fn move_by(&mut self, dx: i32, dy: i32, _held: u8, edge: Option<u8>) -> Option<u16> {
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: a plain read of the cursor.
            unsafe { GetCursorPos(&mut p) };
            let (b, p) = (bounds(), (f64::from(p.x), f64::from(p.y)));
            move_to(b, p.0 + f64::from(dx), p.1 + f64::from(dy));
            edge.and_then(|side| super::past(b, side, p, dx.into(), dy.into()))
        }

        fn enter(&mut self, side: u8, at: u16) {
            let b = bounds();
            let (x, y) = super::entry(b, side, at);
            move_to(b, x, y);
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
            // 한/영 and 한자 by virtual key: as scan codes they mean something only to some
            // layouts (0x72 is no key in KBDKOR.DLL), and the IME acts on the virtual key.
            if let Some(vk) = [(0x90, VK_HANGUL), (0x91, VK_HANJA)].iter().find(|l| l.0 == hid).map(|l| l.1) {
                if down {
                    crate::log(format_args!("input: {} from the peer", if hid == 0x90 { "한/영" } else { "한자" }));
                }
                // With Win or Alt held (a Mac's ⌃Space with Control sent as Win), the key alone
                // going up next would open Start or the menu bar: an unassigned key in between
                // makes it a combination, as AutoHotkey's menu mask does.
                // SAFETY: plain reads of the keyboard state.
                let held = [VK_LWIN, VK_RWIN, VK_MENU].iter().any(|&k| unsafe { GetAsyncKeyState(k.into()) } < 0);
                if down && held {
                    self.mask();
                }
                let input = INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if down { 0 } else { KEYEVENTF_KEYUP }, time: 0, dwExtraInfo: MARK } },
                };
                push(&input);
                return;
            }
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
            push(&input);
        }

        fn mask(&mut self) {
            for up in [0, KEYEVENTF_KEYUP] {
                let mask = INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: MASK_KEY, wScan: 0, dwFlags: up, time: 0, dwExtraInfo: MARK } },
                };
                push(&mask);
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
impl Sink for Os {
    fn move_by(&mut self, _: i32, _: i32, _: u8, _: Option<u8>) -> Option<u16> {
        None
    }
    fn enter(&mut self, _: u8, _: u16) {}
    fn button(&mut self, _: u8, _: bool, _: u8) {}
    fn wheel(&mut self, _: i32, _: i32) {}
    fn key(&mut self, _: u16, _: bool) {}
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records what it is given; its pointer moves on a 100 × 100 screen.
    #[derive(Clone, Default)]
    pub struct Rec(pub Arc<Mutex<Vec<String>>>, Arc<Mutex<(f64, f64)>>);

    pub const SCREEN: Bounds = Bounds { x0: 0.0, y0: 0.0, x1: 100.0, y1: 100.0 };

    impl Sink for Rec {
        fn move_by(&mut self, dx: i32, dy: i32, held: u8, edge: Option<u8>) -> Option<u16> {
            self.0.lock().unwrap().push(format!("move {dx} {dy} held {held}"));
            let mut p = self.1.lock().unwrap();
            let out = edge.and_then(|side| past(SCREEN, side, *p, dx.into(), dy.into()));
            *p = ((p.0 + f64::from(dx)).clamp(0.0, 99.0), (p.1 + f64::from(dy)).clamp(0.0, 99.0));
            out
        }
        fn enter(&mut self, side: u8, at: u16) {
            let p = entry(SCREEN, side, at);
            *self.1.lock().unwrap() = p;
            self.0.lock().unwrap().push(format!("enter at {},{}", p.0, p.1));
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
        fn mask(&mut self) {
            self.0.lock().unwrap().push("mask".into());
        }
    }

    /// A Windows key still held when the driver leaves (Ctrl sent as Win, then the escape key)
    /// goes up masked; one the driver let go of itself, as for Start, isn't masked.
    #[test]
    fn a_windows_key_left_held_goes_up_masked() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        for (hid, down) in [(0xE3, true), (0xE3, false)] {
            inj.apply(Frame::KvmKey { hid, down });
        }
        inj.apply(Frame::KvmLeave);
        for hid in [0xE3, 0xE2, 0xE1] {
            inj.apply(Frame::KvmKey { hid, down: true });
        }
        inj.apply(Frame::KvmLeave);
        assert_eq!(*rec.0.lock().unwrap(), ["key 0xe3 down", "key 0xe3 up", "key 0xe3 down", "key 0xe2 down", "key 0xe1 down", "mask", "key 0xe1 up", "key 0xe2 up", "key 0xe3 up"]);
    }

    /// A key's own flags, as a clean event source gives them (measured 2026-11-22): the function
    /// flag on F-keys and the keys around the arrows, the keypad flag on the keypad and arrows.
    /// Injected events get exactly these, never ones left over in the session.
    #[cfg(target_os = "macos")]
    #[test]
    fn keys_carry_only_their_own_function_and_keypad_flags() {
        use mac::{own_flags, FN, NUMPAD};
        assert_eq!(own_flags(0x03), 0, "f");
        assert_eq!(own_flags(0x50), FN, "F19");
        assert_eq!(own_flags(0x7A), FN, "F1");
        assert_eq!(own_flags(0x7C), FN | NUMPAD, "right arrow");
        assert_eq!(own_flags(0x75), FN, "forward delete");
        assert_eq!(own_flags(0x53), NUMPAD, "keypad 1");
        assert_eq!(own_flags(0x24), 0, "return");
    }

    /// 한/영 on a Mac is whichever input source shortcut is on: this Mac's own setup (Caps Lock
    /// remapped to F18 for "next", F19 for "previous"), one switched off, and none set.
    #[test]
    fn the_input_source_shortcut_is_the_first_one_on() {
        assert_eq!(input_sources([Some((true, 80, 0)), Some((true, 79, 0))]), [Some((80, 0)), Some((79, 0))]);
        assert_eq!(input_sources([Some((false, 49, 0x4_0000)), Some((true, 79, 0))]), [None, Some((79, 0))]);
        assert_eq!(input_sources([Some((true, 65535, 0)), None]), [None, Some((0x31, 0xC_0000))]);
        assert_eq!(input_sources([None, None]), [Some((0x31, 0x4_0000)), Some((0x31, 0xC_0000))]);
    }

    /// Every key has one wire code and back, and this OS injects every key it has: one the
    /// table names that the OS has but can't be given would be dropped at the receiver unnoticed.
    #[test]
    fn every_key_round_trips_and_this_os_can_inject_the_ones_it_has() {
        use KeyCode as K;
        // Keys with no place on this OS's keyboards; anything else must have a code. (A Mac
        // receiver turns Lang1 and Lang2 into shortcuts instead: `mac::Os::key`.)
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

    /// 한/영 and 한자 switch something each time they go down: a repeat would switch it back.
    #[test]
    fn hangul_and_hanja_do_not_repeat() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        for (hid, down) in [(0x90, true), (0x90, true), (0x90, false), (0x91, true), (0x91, true)] {
            inj.apply(Frame::KvmKey { hid, down });
        }
        drop(inj);
        assert_eq!(*rec.0.lock().unwrap(), ["key 0x90 down", "key 0x90 up", "key 0x91 down", "key 0x91 up"]);
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

    #[test]
    fn the_entry_point_is_on_the_edge_and_one_push_out_leaves_where_it_came_in() {
        let b = Bounds { x0: -1920.0, y0: 0.0, x1: 0.0, y1: 1080.0 };
        assert_eq!(entry(b, 0, 0), (-1920.0, 0.0));
        assert_eq!(entry(b, 1, 65535), (-1.0, 1079.0));
        assert_eq!(entry(b, 3, 32768), (-960.0, 1079.0));
        for (side, at) in [(0, 0), (1, 12345), (2, 65535), (3, 40000)] {
            let p = entry(b, side, at);
            let (dx, dy) = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)][side as usize];
            let back = past(b, side, p, dx, dy).expect("one pixel out leaves");
            assert!(back.abs_diff(at) < 65535 / 1000, "side {side}: in at {at}, out at {back}");
            assert_eq!(past(b, side, p, -dx, -dy), None, "a move inward stays");
            assert_eq!(past(b, side, p, dy, dx), None, "a move along the edge stays");
        }
        // The other edges are no way back.
        assert_eq!(past(b, 0, (-1.0, 500.0), 5.0, 0.0), None);
    }

    /// A crossing starts at the edge it entered by, moves from there, and going back out through
    /// it answers once with where; moving out through another edge doesn't.
    #[test]
    fn a_crossing_returns_once_through_the_edge_it_came_in_by() {
        let rec = Rec::default();
        let mut inj = Injector::new(rec.clone());
        assert_eq!(inj.apply(Frame::KvmEnter { route: 5, side: 0, at: 65535 / 2 }), None);
        assert_eq!(inj.apply(mv(5, 1, 30, 0)), None);
        assert_eq!(inj.apply(mv(5, 2, 300, 0)), None, "out through the right edge: no way back");
        assert_eq!(inj.apply(mv(5, 3, 330, 40)), None);
        assert_eq!(inj.apply(mv(5, 4, 231, 40)), None, "back to x 0 (from 99: the right edge held it)");
        assert_eq!(inj.apply(mv(5, 5, 228, 40)), Some(Frame::KvmReturn { route: 5, at: frac(89.0, 0.0, 100.0) }));
        assert_eq!(inj.apply(mv(5, 6, 220, 40)), None, "answered once");
        assert_eq!(rec.0.lock().unwrap()[0], "enter at 0,49");
        // The pad's route has no entry edge: never answers.
        inj.apply(Frame::KvmLeave);
        assert_eq!(inj.apply(mv(6, 1, -500, 0)), None);
    }

    #[test]
    fn the_escape_key_is_ctrl_alt_shift_esc_on_either_side() {
        assert!(escape(0x29, 0x07), "left Ctrl, Shift, Alt");
        assert!(escape(0x29, 0x70 | 0x08), "right ones, ⌘ too");
        assert!(!escape(0x29, 0x03) && !escape(0x29, 0x05) && !escape(0x29, 0x06), "two of three");
        assert!(!escape(0x2A, 0x07), "another key");
    }

    /// A modifier's flagsChanged says which side by its own bit: one Shift let go while the
    /// other is held is an up, though the Shift flag stays.
    #[test]
    fn modifier_changes_say_which_side_and_which_way() {
        assert_eq!(mac_modifier(0x38, 0x2_0002), Some((0xE1, true)));
        assert_eq!(mac_modifier(0x38, 0x2_0004), Some((0xE1, false)), "left up, right still held");
        assert_eq!(mac_modifier(0x3C, 0x2_0004), Some((0xE5, true)));
        assert_eq!(mac_modifier(0x36, 0x10_0010), Some((0xE7, true)));
        assert_eq!(mac_modifier(0x3E, 0x4_0000), Some((0xE4, false)), "only the left Control's bit");
        assert_eq!(mac_modifier(0x00, 0x2_0002), None, "not a modifier");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn this_os_s_codes_map_back_to_their_keys() {
        for &(_, h) in KEYS {
            if let Some(c) = scancode(h) {
                assert_eq!(hid_of(c), Some(h), "{h:#x} is {c:#x} here");
            }
        }
    }

    /// Each crossing is a new route entered by the edge facing this device, and its moves are
    /// totals from where it entered.
    #[test]
    fn crossings_number_their_routes_and_total_their_moves() {
        let mut d = Driver::default();
        assert_eq!(d.frame(Captured::Enter(100), 1), Frame::KvmEnter { route: 1, side: 0, at: 100 });
        assert_eq!(d.frame(Captured::Move(3, -1), 1), mv(1, 1, 3, -1));
        assert_eq!(d.frame(Captured::Move(4, 0), 1), mv(1, 2, 7, -1));
        assert_eq!(d.frame(Captured::Wheel(0, 99_999), 1), Frame::KvmWheel { dx: 0, dy: i16::MAX });
        assert_eq!(d.frame(Captured::Leave, 1), Frame::KvmLeave);
        assert_eq!(d.frame(Captured::Enter(7), 2), Frame::KvmEnter { route: 2, side: 3, at: 7 });
        assert_eq!(d.frame(Captured::Move(1, 1), 2), mv(2, 1, 1, 1));
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
