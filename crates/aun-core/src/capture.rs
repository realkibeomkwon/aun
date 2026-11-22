//! macOS: crossing over to the peer past a screen edge with this Mac's own mouse and keyboard
//! (docs/kvm-design.md 1, 2 and 7). An event tap watches the pointer; once it pushes out past
//! the edge toward the peer, the tap swallows every mouse and key event and a sender thread
//! turns them into `Kvm*` frames, until the pointer comes back (`KvmReturn`), the escape key,
//! a stall, or the link going.
//!
//! What matters most is that this Mac gets its input back. The tap callback only reads the event
//! and queues it (atomics and a `try_send`, no lock, no log); every way out goes through
//! [`State::end`], which can run on any thread and twice.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::kvm::{self, Bounds, Captured, Driver};
use crate::store::Id;
use crate::Node;

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
/// Captured events waiting for the sender thread; full, the crossing ends (docs 7.4).
const QUEUE: usize = 1024;
/// How far from the edge the pointer must go before a crossing that ended without coming back
/// (escape key, a stall) may start again: pushing on, it would only cross into the same stall.
const REARM: f64 = 50.0;

/// What the tap callback and the rest share. Lives as long as the tap (see [`Cross`]'s drop).
struct State {
    /// Where the peer is from here, as in `KvmEnter`.
    side: u8,
    out: SyncSender<Captured>,
    crossed: AtomicBool,
    /// A crossing may start: a peer to cross to, and back from the edge since the last one ended
    /// without coming back.
    peer: AtomicBool,
    armed: AtomicBool,
    /// Crossings ended here, counted: the sender owes one `KvmLeave` for each it started.
    ends: AtomicU32,
    /// Where along the edge the last crossing went out, to put the pointer back there.
    at: AtomicU32,
    /// The screens' box as last read, as `f64` bits: read again when the pointer seems past it.
    bounds: [AtomicU64; 4],
    tap: AtomicPtr<c_void>,
}

impl State {
    fn bounds(&self) -> Bounds {
        let [x0, y0, x1, y1] = self.bounds.each_ref().map(|b| f64::from_bits(b.load(Ordering::Relaxed)));
        Bounds { x0, y0, x1, y1 }
    }

    fn reread(&self) -> Bounds {
        let b = kvm::screen_bounds().unwrap_or(self.bounds());
        for (a, v) in self.bounds.iter().zip([b.x0, b.y0, b.x1, b.y1]) {
            a.store(v.to_bits(), Ordering::Relaxed);
        }
        b
    }

    /// Ends the crossing, if one is on: gives the pointer back at the edge, `at` along it if it
    /// came back there (`KvmReturn`), else where it went out, and owes the peer a `KvmLeave`.
    /// Without `at` (escape, a stall, the link gone) a new crossing waits until the pointer has
    /// moved away from the edge.
    fn end(&self, at: Option<u16>) {
        if !self.crossed.swap(false, Ordering::SeqCst) {
            return;
        }
        self.armed.store(at.is_some(), Ordering::Relaxed);
        let at = at.unwrap_or(self.at.load(Ordering::Relaxed) as u16);
        let (x, y) = kvm::entry(self.reread(), self.side, at);
        // SAFETY: plain calls taking values.
        unsafe {
            CGWarpMouseCursorPosition(Point { x, y });
            CGAssociateMouseAndMouseCursorPosition(true);
        }
        self.ends.fetch_add(1, Ordering::SeqCst);
    }

    /// How far the pointer at `p` is from the edge toward the peer.
    fn edge_distance(&self, b: Bounds, p: Point) -> f64 {
        match self.side {
            0 => p.x - b.x0,
            1 => b.x1 - 1.0 - p.x,
            2 => p.y - b.y0,
            _ => b.y1 - 1.0 - p.y,
        }
    }

    fn send(&self, c: Captured) -> bool {
        if self.out.try_send(c).is_ok() {
            return true;
        }
        self.end(None); // the sender is stuck: input back here (docs 7.4)
        false
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
        st.end(None);
        // SAFETY: the tap this callback belongs to.
        unsafe { CGEventTapEnable(st.tap.load(Ordering::Relaxed), true) };
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
            if st.edge_distance(st.bounds(), p) >= REARM {
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
        st.crossed.store(true, Ordering::SeqCst);
        // SAFETY: a plain call. The pointer stays where it is; the deltas still come.
        unsafe { CGAssociateMouseAndMouseCursorPosition(false) };
        return std::ptr::null_mut();
    }
    // SAFETY: as above.
    let flags = unsafe { CGEventGetFlags(e) };
    let c = match kind {
        MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED => Captured::Move(field(DELTA_X) as i32, field(DELTA_Y) as i32),
        LEFT_DOWN | LEFT_UP => Captured::Button(0, kind == LEFT_DOWN, field(CLICK_STATE).clamp(1, 255) as u8),
        RIGHT_DOWN | RIGHT_UP => Captured::Button(1, kind == RIGHT_DOWN, field(CLICK_STATE).clamp(1, 255) as u8),
        OTHER_DOWN | OTHER_UP if field(BUTTON_NUMBER) == 2 => Captured::Button(2, kind == OTHER_DOWN, field(CLICK_STATE).clamp(1, 255) as u8),
        SCROLL => Captured::Wheel(field(SCROLL_POINT_X) as i32, field(SCROLL_POINT_Y) as i32),
        KEY_DOWN | KEY_UP => {
            let Some(hid) = kvm::hid_of(field(KEYCODE) as u32) else { return std::ptr::null_mut() };
            if kind == KEY_DOWN && kvm::escape(hid, mods(flags)) {
                st.end(None);
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

/// Crossing to a peer with this Mac's mouse and keyboard: the tap, and the thread sending what
/// it takes. Dropped, the tap goes and input is back here.
pub struct Cross {
    st: Arc<State>,
    /// The tap thread's run loop, to stop it.
    rl: usize,
    target: Arc<Mutex<Option<Id>>>,
    route: Arc<AtomicU32>,
    swap: Arc<AtomicBool>,
}

impl Cross {
    /// Starts watching for the pointer at the `side` edge (0 left, 1 right, 2 top, 3 bottom).
    /// Err when macOS doesn't let Aun see input: Input Monitoring asked for, or not granted.
    pub fn start(node: Node, side: u8, swap: bool) -> Result<Cross, String> {
        // SAFETY: plain calls.
        let listen = unsafe { CGPreflightListenEventAccess() };
        crate::log(format_args!("cross: Input Monitoring granted: {listen}, Accessibility: {}", kvm::allowed(false)));
        // SAFETY: a plain call; it opens the system's request the first time.
        if !listen && !unsafe { CGRequestListenEventAccess() } {
            return Err("Input Monitoring not granted".into());
        }
        let (out, rx) = mpsc::sync_channel(QUEUE);
        let b = kvm::screen_bounds().ok_or("no display")?;
        let st = Arc::new(State {
            side,
            out,
            crossed: AtomicBool::new(false),
            peer: AtomicBool::new(false),
            armed: AtomicBool::new(true),
            ends: AtomicU32::new(0),
            at: AtomicU32::new(0),
            bounds: [b.x0, b.y0, b.x1, b.y1].map(|v| AtomicU64::new(v.to_bits())),
            tap: AtomicPtr::new(std::ptr::null_mut()),
        });
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
                (*(user as *const State)).tap.store(port, Ordering::Relaxed);
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
                st.end(None);
            }
        }).map_err(|e| e.to_string())?;
        let rl = made.recv().ok().flatten().ok_or("event tap not made (a permission missing?)")?;
        let cross = Cross { st: st.clone(), rl, target: Default::default(), route: Default::default(), swap: Arc::new(AtomicBool::new(swap)) };
        let (target, route, swap, weak) = (cross.target.clone(), cross.route.clone(), cross.swap.clone(), Arc::downgrade(&st));
        drop(st); // the sender holds it weakly: gone with the tap, its queue closes and it ends
        std::thread::Builder::new().name("aun-cross".into()).spawn(move || sender(rx, &weak, &node, &target, &route, &swap)).map_err(|e| e.to_string())?;
        crate::log(format_args!("cross: watching the {} edge", ["left", "right", "top", "bottom"][usize::from(side.min(3))]));
        Ok(cross)
    }

    /// The peer to cross to; None stops crossing (and ends one under way).
    pub fn set_peer(&self, id: Option<Id>) {
        *self.target.lock().unwrap() = id;
        self.st.peer.store(id.is_some(), Ordering::Relaxed);
        if id.is_none() {
            self.st.end(None);
        }
    }

    pub fn set_swap(&self, on: bool) {
        self.swap.store(on, Ordering::Relaxed);
    }

    /// The pointer came back from `id` (`Note::Returned`): input back here at that point.
    pub fn returned(&self, id: Id, route: u32, at: u16) {
        if *self.target.lock().unwrap() == Some(id) && self.route.load(Ordering::SeqCst) == route {
            self.st.end(Some(at));
        }
    }

    /// `id` stopped answering (`Note::InputLost`): input back here. True if that ended a crossing.
    pub fn lost(&self, id: Id) -> bool {
        let across = self.st.crossed.load(Ordering::SeqCst);
        if *self.target.lock().unwrap() == Some(id) {
            self.st.end(None);
            return across;
        }
        false
    }
}

impl Drop for Cross {
    fn drop(&mut self) {
        self.st.peer.store(false, Ordering::Relaxed);
        self.st.end(None);
        // SAFETY: the tap (alive until its thread invalidates it after the run loop stops), then
        // that thread's run loop. Off first, so a stop that lands before the loop runs leaves no
        // live tap.
        unsafe {
            CGEventTapEnable(self.st.tap.load(Ordering::Relaxed), false);
            CFRunLoopStop(self.rl as Ref);
        }
    }
}

/// Puts the cursor back with the mouse, for a panic or a quit while across: whether macOS does
/// it when the process ends is not known (docs 7.5).
pub fn release_cursor() {
    // SAFETY: a plain call.
    unsafe { CGAssociateMouseAndMouseCursorPosition(true) };
}

/// Sends what the tap took, in order, and one `KvmLeave` per crossing that ended. Wakes every
/// 50 ms while across to see an end, asleep otherwise.
fn sender(rx: Receiver<Captured>, st: &Weak<State>, node: &Node, target: &Mutex<Option<Id>>, route: &AtomicU32, swap: &AtomicBool) {
    let mut d = Driver::default();
    let (mut started, mut left) = (0u32, 0u32);
    let mut to: Option<Id> = None;
    let leave = |to: Option<Id>, left: &mut u32| {
        *left += 1;
        if let Some(id) = to {
            node.send_input(id, crate::wire::Frame::KvmLeave);
        }
    };
    loop {
        let first = if started != left {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        let Some(st) = st.upgrade() else { return };
        for c in first.into_iter().chain(rx.try_iter()) {
            if let Captured::Enter(_) = c {
                if started != left {
                    leave(to, &mut left); // the previous one ended before this could start
                }
                started += 1;
                to = *target.lock().unwrap();
            }
            let Some(id) = to else {
                st.end(None);
                continue;
            };
            let f = match d.frame(c, st.side) {
                crate::wire::Frame::KvmKey { hid, down } => crate::wire::Frame::KvmKey { hid: kvm::swapped(hid, swap.load(Ordering::Relaxed)), down },
                f => f,
            };
            if let crate::wire::Frame::KvmEnter { route: r, .. } = f {
                route.store(r, Ordering::SeqCst);
            }
            // A move or scroll dropped on a full queue is no reason: the next move carries it.
            let must = f.must_arrive();
            if !node.send_input(id, f) && must {
                st.end(None); // no link that speaks input, or stuck: nothing to drive
            }
        }
        if started != left && st.ends.load(Ordering::SeqCst) > left {
            leave(to, &mut left);
        }
    }
}
