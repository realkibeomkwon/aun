//! Crossing over to the peer past a screen edge with this device's own mouse and keyboard
//! (docs/kvm-design.md 1, 2 and 7). A tap (macOS) or low-level hooks (Windows) watch the
//! pointer; once it pushes out past the edge toward the peer, they swallow every mouse and key
//! event and a sender thread turns them into `Kvm*` frames, until the pointer comes back
//! (`KvmReturn`), the escape key, a stall, or the link going.
//!
//! What matters most is that this device gets its input back. The callback only reads the event
//! and queues it (atomics and a `try_send`, no lock, no log); every way out goes through
//! [`State::end`], which can run on any thread and twice.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::kvm::{self, Bounds, Captured, Driver};
use crate::store::Id;
use crate::Node;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as os;
#[cfg(target_os = "macos")]
pub use mac::release_cursor;
#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as os;

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
    /// Why the last crossing ended ([`Why`]), for the sender's log.
    why: AtomicU8,
    /// Where the cursor is kept while across.
    lock: [AtomicU64; 2],
    #[cfg(target_os = "macos")]
    os: mac::Shared,
    #[cfg(windows)]
    os: win::Shared,
}

/// Why a crossing ended, logged by the sender (the tap callback may not log).
struct Why;
impl Why {
    const RETURNED: u8 = 0;
    const ESCAPE: u8 = 1;
    const QUEUE_FULL: u8 = 2;
    #[cfg(target_os = "macos")]
    const TAP_TIMEOUT: u8 = 3;
    #[cfg(target_os = "macos")]
    const TAP_USER: u8 = 4;
    const LOST: u8 = 5;
    const NO_PEER: u8 = 6;
    const NOT_SENT: u8 = 7;
    const STOPPED: u8 = 8;
    const BLOCKED: u8 = 9;
    const NAMES: [&str; 10] = ["came back", "escape key", "queue full", "tap disabled by timeout", "tap disabled by user input", "no word back", "no peer", "a frame not sent", "stopped", "blocked there"];
}

impl State {
    fn new(side: u8, out: SyncSender<Captured>, b: Bounds) -> State {
        State {
            side,
            out,
            crossed: AtomicBool::new(false),
            peer: AtomicBool::new(false),
            armed: AtomicBool::new(true),
            ends: AtomicU32::new(0),
            at: AtomicU32::new(0),
            bounds: [b.x0, b.y0, b.x1, b.y1].map(|v| AtomicU64::new(v.to_bits())),
            why: AtomicU8::new(Why::STOPPED),
            lock: [AtomicU64::new(0), AtomicU64::new(0)],
            #[cfg(target_os = "macos")]
            os: mac::Shared::new(),
            #[cfg(windows)]
            os: win::Shared::default(),
        }
    }

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

    fn lock_at(&self, (x, y): (f64, f64)) {
        self.lock.iter().zip([x, y]).for_each(|(a, v)| a.store(v.to_bits(), Ordering::Relaxed));
    }

    fn locked(&self) -> (f64, f64) {
        let [x, y] = self.lock.each_ref().map(|a| f64::from_bits(a.load(Ordering::Relaxed)));
        (x, y)
    }

    /// Ends the crossing, if one is on: gives the pointer back at the edge, `at` along it if it
    /// came back there (`KvmReturn`), else where it went out, and owes the peer a `KvmLeave`.
    /// Without `at` (escape, a stall, the link gone) a new crossing waits until the pointer has
    /// moved away from the edge.
    fn end(&self, at: Option<u16>, why: u8) {
        if !self.crossed.swap(false, Ordering::SeqCst) {
            return;
        }
        self.why.store(why, Ordering::Relaxed);
        self.armed.store(at.is_some(), Ordering::Relaxed);
        let at = at.unwrap_or(self.at.load(Ordering::Relaxed) as u16);
        os::give_back(kvm::entry(self.reread(), self.side, at));
        self.ends.fetch_add(1, Ordering::SeqCst);
    }

    /// How far the pointer at `p` is from the edge toward the peer.
    fn edge_distance(&self, b: Bounds, p: (f64, f64)) -> f64 {
        distance(b, self.side, p)
    }

    fn send(&self, c: Captured) -> bool {
        if self.out.try_send(c).is_ok() {
            return true;
        }
        self.end(None, Why::QUEUE_FULL); // the sender is stuck: input back here (docs 7.4)
        false
    }
}

/// How far `(x, y)` is from `side`'s edge of `b`: 0 on its pixels, below 0 past it.
fn distance(b: Bounds, side: u8, (x, y): (f64, f64)) -> f64 {
    match side {
        0 => x - b.x0,
        1 => b.x1 - 1.0 - x,
        2 => y - b.y0,
        _ => b.y1 - 1.0 - y,
    }
}

/// Windows: where along `side`'s edge a move from the cursor at `from` to `to` pushes out past
/// it. The low-level hook hands over where the pointer would go, which Windows may already have
/// kept on the screens (unverified): so a push is a move from the edge's pixels to them or past.
/// shortcut: a move along the edge while on it crosses too; Raw Input deltas would tell.
#[cfg_attr(not(windows), allow(dead_code))]
fn pushed(b: Bounds, side: u8, from: (f64, f64), to: (f64, f64)) -> Option<u16> {
    if distance(b, side, from) > 0.0 || distance(b, side, to) > 0.0 {
        return None;
    }
    let (dx, dy) = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)][usize::from(side.min(3))];
    kvm::past(b, side, (to.0.clamp(b.x0, b.x1 - 1.0), to.1.clamp(b.y0, b.y1 - 1.0)), dx, dy)
}

/// Windows: the click count of a button going down at `time` (ms), given the last down as
/// `(button, time, count)`: one more when the same button comes again within `within` ms. The
/// cursor stays put while across, so distance can't tell, unlike Windows' own count.
#[cfg_attr(not(windows), allow(dead_code))]
fn clicks(last: &mut (u8, u32, u8), button: u8, time: u32, within: u32) -> u8 {
    let n = if last.2 > 0 && last.0 == button && time.wrapping_sub(last.1) <= within { last.2.saturating_add(1) } else { 1 };
    *last = (button, time, n);
    n
}

/// Windows: wheel units (120 a notch) as wire pixels, keeping what's left over in `rest` so a
/// touchpad's small steps add up.
#[cfg_attr(not(windows), allow(dead_code))]
fn wheel_px(rest: &mut i32, units: i32) -> i32 {
    /// shortcut: the receiver's guess at a notch, `PX_PER_NOTCH` in kvm.rs; tune on the device.
    const PX_PER_NOTCH: i32 = 40;
    *rest += units * PX_PER_NOTCH;
    let px = *rest / 120;
    *rest -= px * 120;
    px
}

/// Crossing to a peer with this device's mouse and keyboard: the tap, and the thread sending
/// what it takes. Dropped, the tap goes and input is back here.
pub struct Cross {
    st: Arc<State>,
    /// What stops the tap's thread (macOS: its run loop, Windows: its thread id).
    stop: usize,
    target: Arc<Mutex<Option<Id>>>,
    route: Arc<AtomicU32>,
    swap: Arc<AtomicBool>,
}

impl Cross {
    /// Starts watching for the pointer at the `side` edge (0 left, 1 right, 2 top, 3 bottom).
    /// Err when the OS doesn't let Aun see input (macOS: Input Monitoring asked for, or not
    /// granted) or the tap can't be made.
    pub fn start(node: Node, side: u8, swap: bool) -> Result<Cross, String> {
        let (out, rx) = mpsc::sync_channel(QUEUE);
        let b = kvm::screen_bounds().ok_or("no display")?;
        let st = Arc::new(State::new(side, out, b));
        let stop = os::start(&st)?;
        let cross = Cross { st: st.clone(), stop, target: Default::default(), route: Default::default(), swap: Arc::new(AtomicBool::new(swap)) };
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
            self.st.end(None, Why::NO_PEER);
        }
    }

    pub fn set_swap(&self, on: bool) {
        self.swap.store(on, Ordering::Relaxed);
    }

    /// The pointer came back from `id` (`Note::Returned`): input back here at that point.
    pub fn returned(&self, id: Id, route: u32, at: u16) {
        if *self.target.lock().unwrap() == Some(id) && self.route.load(Ordering::SeqCst) == route {
            self.st.end(Some(at), Why::RETURNED);
        }
    }

    /// `id` can't take input now (`Note::Blocked`): input back here. True if that ended a crossing.
    pub fn blocked(&self, id: Id, route: u32) -> bool {
        let across = self.st.crossed.load(Ordering::SeqCst);
        if across && *self.target.lock().unwrap() == Some(id) && self.route.load(Ordering::SeqCst) == route {
            self.st.end(None, Why::BLOCKED);
            return true;
        }
        false
    }

    /// `id` stopped answering (`Note::InputLost`): input back here. True if that ended a crossing.
    pub fn lost(&self, id: Id) -> bool {
        let across = self.st.crossed.load(Ordering::SeqCst);
        if *self.target.lock().unwrap() == Some(id) {
            self.st.end(None, Why::LOST);
            return across;
        }
        false
    }
}

impl Drop for Cross {
    fn drop(&mut self) {
        self.st.peer.store(false, Ordering::Relaxed);
        self.st.end(None, Why::STOPPED);
        os::stop(&self.st, self.stop);
    }
}

/// Sends what the tap took, in order, and one `KvmLeave` per crossing that ended. Wakes every
/// 50 ms while across to see an end, asleep otherwise.
fn sender(rx: Receiver<Captured>, st: &Weak<State>, node: &Node, target: &Mutex<Option<Id>>, route: &AtomicU32, swap: &AtomicBool) {
    let mut d = Driver::default();
    let (mut started, mut left) = (0u32, 0u32);
    let mut to: Option<Id> = None;
    // Moves, keys and scrolls taken in this crossing, for the log (2026-11-26: typing and touchpad
    // scrolling across from the PC didn't land; this says whether the hook saw them).
    let mut taken = [0u32; 3];
    // The most this side's send buffer held while across, sampled every 50 ms (macOS): whether a
    // file going out held the input back (2026-11-27, docs/kvm-design.md 8, 4-0).
    let (mut unsent, mut sampled): (Option<usize>, Option<Instant>) = (None, None);
    let leave = |to: Option<Id>, left: &mut u32, st: &State, taken: &mut [u32; 3], unsent: &mut Option<usize>| {
        let why = Why::NAMES[usize::from(st.why.load(Ordering::Relaxed)).min(Why::NAMES.len() - 1)];
        let buffered = unsent.take().map_or(String::new(), |b| format!(", at most {} KB in the send buffer", b >> 10));
        crate::log(format_args!("cross: out ({why}; took {} moves, {} keys, {} scrolls{buffered}{})", taken[0], taken[1], taken[2], os::diag(st)));
        *taken = [0; 3];
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
                    leave(to, &mut left, &st, &mut taken, &mut unsent); // the previous one ended before this could start
                }
                started += 1;
                to = *target.lock().unwrap();
                os::entered(&st);
                let (x, y) = os::cursor();
                let b = st.bounds();
                crate::log(format_args!("cross: in at {x:.0},{y:.0} (screens {:.0},{:.0} to {:.0},{:.0})", b.x0, b.y0, b.x1, b.y1));
            }
            match c {
                Captured::Move(..) => taken[0] += 1,
                Captured::Key(..) => taken[1] += 1,
                Captured::Wheel(..) => taken[2] += 1,
                _ => {}
            }
            let Some(id) = to else {
                st.end(None, Why::NO_PEER);
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
                st.end(None, Why::NOT_SENT); // no link that speaks input, or stuck: nothing to drive
            }
        }
        if started != left
            && let Some(id) = to
            && sampled.is_none_or(|t| t.elapsed() >= Duration::from_millis(50))
        {
            sampled = Some(Instant::now());
            if let Some(b) = node.unsent(id) {
                unsent = Some(unsent.unwrap_or(0).max(b));
            }
        }
        if started != left && st.ends.load(Ordering::SeqCst) > left {
            leave(to, &mut left, &st, &mut taken, &mut unsent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Bounds = Bounds { x0: 0.0, y0: 0.0, x1: 1920.0, y1: 1080.0 };

    #[test]
    fn a_push_crosses_only_from_the_edge() {
        // Peer on the left: on the edge and pushed (kept on it, or past it) goes, halfway down.
        assert_eq!(pushed(B, 0, (0.0, 540.0), (0.0, 540.0)), Some(32_798));
        assert_eq!(pushed(B, 0, (0.0, 540.0), (-7.0, 540.0)), Some(32_798));
        // Arriving at the edge is not yet a push; nor is leaving it, nor another edge.
        assert_eq!(pushed(B, 0, (5.0, 540.0), (0.0, 540.0)), None);
        assert_eq!(pushed(B, 0, (0.0, 540.0), (3.0, 540.0)), None);
        assert_eq!(pushed(B, 0, (1919.0, 540.0), (1919.0, 540.0)), None);
        // Peer on the right: the last pixel column is the edge.
        assert_eq!(pushed(B, 1, (1919.0, 0.0), (1919.0, 0.0)), Some(0));
        assert_eq!(pushed(B, 1, (1918.0, 0.0), (1919.0, 0.0)), None);
    }

    #[test]
    fn clicks_count_up_only_for_the_same_button_in_time() {
        let mut last = (0, 0, 0);
        assert_eq!(clicks(&mut last, 0, 1000, 500), 1);
        assert_eq!(clicks(&mut last, 0, 1400, 500), 2);
        assert_eq!(clicks(&mut last, 0, 1800, 500), 3);
        assert_eq!(clicks(&mut last, 0, 2400, 500), 1, "too late");
        assert_eq!(clicks(&mut last, 1, 2500, 500), 1, "another button");
        // The tick count wraps after 49 days.
        let mut last = (0, u32::MAX - 100, 1);
        assert_eq!(clicks(&mut last, 0, 100, 500), 2);
    }

    #[test]
    fn small_wheel_steps_add_up_and_notches_go_whole() {
        let mut rest = 0;
        assert_eq!(wheel_px(&mut rest, 120), 40);
        assert_eq!(wheel_px(&mut rest, -240), -80);
        // A touchpad's 1-unit steps: nothing for two, then a pixel on the third.
        assert_eq!([1, 1, 1].map(|u| wheel_px(&mut rest, u)), [0, 0, 1]);
    }
}
