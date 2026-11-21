//! Mouse input from a paired device, injected here (docs/kvm-design.md, step 1). Nothing is
//! captured: the driver is a trackpad area in the other device's Aun window.
//!
//! What matters most is that nothing stays held: a button the driver pressed is released when it
//! lets go, when the link drops, and when this device stops allowing remote control.

use crate::wire::Frame;

/// Where input lands: the OS, or a recorder in tests.
pub trait Sink {
    /// Moves the pointer by pixels; `held` has bit `b` set for each button `b` down.
    fn move_by(&mut self, dx: i32, dy: i32, held: u8);
    fn button(&mut self, button: u8, down: bool, clicks: u8);
    fn wheel(&mut self, dx: i32, dy: i32);
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
}

/// One link's input: the moves applied so far and the buttons it holds down.
pub struct Injector<S: Sink> {
    sink: S,
    /// The route, move number and total of the last move applied.
    last: Option<(u32, u32, i32, i32)>,
    held: u8,
}

impl<S: Sink> Injector<S> {
    pub fn new(sink: S) -> Self {
        Injector { sink, last: None, held: 0 }
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
            _ => {}
        }
    }

    /// Lets go of every button held down here.
    pub fn release(&mut self) {
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
pub struct Os;

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

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: Ref) -> Ref;
        fn CGEventGetLocation(e: Ref) -> Point;
        fn CGEventCreateMouseEvent(source: Ref, kind: u32, at: Point, button: u32) -> Ref;
        fn CGEventCreateScrollWheelEvent2(source: Ref, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> Ref;
        fn CGEventSetIntegerValueField(e: Ref, field: u32, v: i64);
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

    fn post(e: Ref) {
        if !e.is_null() {
            // SAFETY: `e` is an event this module created; posting doesn't take ownership.
            unsafe {
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
                post(e);
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
                post(e);
            }
        }

        fn wheel(&mut self, dx: i32, dy: i32) {
            // SAFETY: as above.
            post(unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null_mut(), PIXEL, 2, dy, dx, 0) });
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
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
impl Sink for Os {
    fn move_by(&mut self, _: i32, _: i32, _: u8) {}
    fn button(&mut self, _: u8, _: bool, _: u8) {}
    fn wheel(&mut self, _: i32, _: i32) {}
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
