//! Windows: the low-level mouse and keyboard hooks behind [`super::Cross`], on their own thread
//! with a message loop (docs/kvm-design.md 3).
//!
//! While across, the cursor is parked mid-screen and every move is swallowed, so it stays there:
//! each move's point less the parked one is the move. Parked on the edge, a push further out
//! would read as no move, if Windows keeps the hook's point on the screens.

use std::cell::Cell;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{REARM, State, Why};
use crate::kvm::{self, Captured};

/// The state the hooks serve: set while they are on (one `Cross` at a time; the app drops the
/// old one first). Hooks take no user pointer.
static ST: AtomicPtr<State> = AtomicPtr::new(null_mut());

thread_local! {
    /// What the hooks keep between events, on their thread: the wire modifiers held while
    /// across (bit `i` for HID `0xE0 + i`), the last button down for [`super::clicks`], and the
    /// wheel left over for [`super::wheel_px`].
    static MODS: Cell<u8> = const { Cell::new(0) };
    static LAST_DOWN: Cell<(u8, u32, u8)> = const { Cell::new((0, 0, 0)) };
    static WHEEL: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
}

/// Diagnostic (2026-11-26 device test: `a` pressed across went nowhere, and the escape key took
/// two presses): keys the hook took but found no wire key for, and the modifiers held at each
/// Esc that didn't end the crossing. Only failures are kept, so nothing typed is.
#[derive(Default)]
pub(super) struct Shared {
    unmapped: AtomicU32,
    /// The last one's virtual key, scan code and flags, 16 bits each.
    last_unmapped: AtomicU64,
    /// The last Esc that didn't count: its count and the modifiers held, as wire bits.
    esc_missed: AtomicU32,
    esc_mods: AtomicU32,
}

/// What [`Shared`] holds for this crossing, for its last log line; cleared for the next.
pub(super) fn diag(st: &State) -> String {
    let d = &st.os;
    let (n, last) = (d.unmapped.swap(0, Ordering::Relaxed), d.last_unmapped.swap(0, Ordering::Relaxed));
    let (esc, mods) = (d.esc_missed.swap(0, Ordering::Relaxed), d.esc_mods.swap(0, Ordering::Relaxed));
    format!("; {n} keys with no wire key, last vk {:#x} scan {:#x} flags {:#x}; {esc} Esc not escaping, last with modifiers {mods:#04x}", last >> 32, (last >> 16) & 0xFFFF, last & 0xFFFF)
}

pub(super) fn cursor() -> (f64, f64) {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: a plain read of the cursor.
    unsafe { GetCursorPos(&mut p) };
    (p.x.into(), p.y.into())
}

/// The cursor back at `(x, y)`: it was never taken from the mouse, only kept still.
pub(super) fn give_back((x, y): (f64, f64)) {
    // SAFETY: a plain call taking values.
    unsafe { SetCursorPos(x as i32, y as i32) };
}

/// Whether a modifier or mouse button is down: then no crossing starts (its up would come here).
fn held() -> bool {
    [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN, VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|&k| {
        // SAFETY: a plain read of the key state.
        (unsafe { GetAsyncKeyState(k.into()) }) < 0
    })
}

const SWALLOW: LRESULT = 1;

unsafe extern "system" fn mouse(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let st = ST.load(Ordering::Acquire);
    if code < 0 || st.is_null() {
        // SAFETY: passing the event on, as the hook contract asks.
        return unsafe { CallNextHookEx(null_mut(), code, w, l) };
    }
    // SAFETY: set while this hook is on, and the `State` outlives it; `l` is the hook's struct.
    let (st, m) = unsafe { (&*st, &*(l as *const MSLLHOOKSTRUCT)) };
    // SAFETY: as above.
    let pass = || unsafe { CallNextHookEx(null_mut(), code, w, l) };
    // What Aun itself injected (a peer driving this PC): never a crossing, never swallowed.
    if m.dwExtraInfo == kvm::MARK as usize {
        return pass();
    }
    let pt = (f64::from(m.pt.x), f64::from(m.pt.y));
    let msg = w as u32;
    if !st.crossed.load(Ordering::SeqCst) {
        // Not while the peer drives this PC: both would be driving each other.
        if msg != WM_MOUSEMOVE || !st.peer.load(Ordering::Relaxed) || crate::node::driven() {
            return pass();
        }
        let b = st.reread();
        if !st.armed.load(Ordering::Relaxed) {
            if st.edge_distance(b, pt) >= REARM {
                st.armed.store(true, Ordering::Relaxed);
            }
            return pass();
        }
        // Held keys last: read only at the edge.
        let Some(at) = super::pushed(b, st.side, cursor(), pt).filter(|_| !held()) else { return pass() };
        // Not queued, it never started: the sender counts crossings by their Enter.
        if st.out.try_send(Captured::Enter(at)).is_err() {
            return pass();
        }
        st.at.store(at.into(), Ordering::Relaxed);
        MODS.set(0);
        WHEEL.set((0, 0));
        // SAFETY: a plain call; the point read back is where it landed (a gap between screens
        // moves it).
        unsafe { SetCursorPos(((b.x0 + b.x1) / 2.0) as i32, ((b.y0 + b.y1) / 2.0) as i32) };
        st.lock_at(cursor());
        st.crossed.store(true, Ordering::SeqCst);
        return SWALLOW;
    }
    let button = |b: u8, down: bool| {
        let n = if down {
            let mut last = LAST_DOWN.get();
            // SAFETY: a plain read.
            let n = super::clicks(&mut last, b, m.time, unsafe { GetDoubleClickTime() });
            LAST_DOWN.set(last);
            n
        } else {
            LAST_DOWN.get().2.max(1)
        };
        Captured::Button(b, down, n)
    };
    let units = i32::from((m.mouseData >> 16) as u16 as i16);
    let c = match msg {
        WM_MOUSEMOVE => {
            let (x, y) = st.locked();
            let (dx, dy) = ((pt.0 - x) as i32, (pt.1 - y) as i32);
            if (dx, dy) == (0, 0) {
                return SWALLOW;
            }
            Captured::Move(dx, dy)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => button(0, msg == WM_LBUTTONDOWN),
        WM_RBUTTONDOWN | WM_RBUTTONUP => button(1, msg == WM_RBUTTONDOWN),
        WM_MBUTTONDOWN | WM_MBUTTONUP => button(2, msg == WM_MBUTTONDOWN),
        // Wire: positive dy scrolls up, positive dx scrolls left (as the receivers read it).
        WM_MOUSEWHEEL => {
            let (rx, mut ry) = WHEEL.get();
            let dy = super::wheel_px(&mut ry, units);
            WHEEL.set((rx, ry));
            Captured::Wheel(0, dy)
        }
        WM_MOUSEHWHEEL => {
            let (mut rx, ry) = WHEEL.get();
            let dx = super::wheel_px(&mut rx, -units);
            WHEEL.set((rx, ry));
            Captured::Wheel(dx, 0)
        }
        _ => return SWALLOW, // side buttons: not on the wire
    };
    if !matches!(c, Captured::Wheel(0, 0)) {
        st.send(c);
    }
    SWALLOW
}

unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let st = ST.load(Ordering::Acquire);
    if code < 0 || st.is_null() {
        // SAFETY: passing the event on, as the hook contract asks.
        return unsafe { CallNextHookEx(null_mut(), code, w, l) };
    }
    // SAFETY: as in `mouse`.
    let (st, k) = unsafe { (&*st, &*(l as *const KBDLLHOOKSTRUCT)) };
    if k.dwExtraInfo == kvm::MARK as usize || !st.crossed.load(Ordering::SeqCst) {
        // SAFETY: as above.
        return unsafe { CallNextHookEx(null_mut(), code, w, l) };
    }
    let down = matches!(w as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    // 한/영 and 한자 by what the layout made of the key; the rest by scan code, read as the pad
    // reads it (winit's forward table): its reverse one gives Lang1 as 0xE0F2 under a Korean
    // layout, so a Caps Lock mapped to 0x72 (this PC) would be no key.
    let hid = match k.vkCode as u16 {
        VK_HANGUL => Some(0x90),
        VK_HANJA => Some(0x91),
        _ => match <winit::keyboard::PhysicalKey as winit::platform::scancode::PhysicalKeyExtScancode>::from_scancode(k.scanCode | if k.flags & LLKHF_EXTENDED != 0 { 0xE000 } else { 0 }) {
            winit::keyboard::PhysicalKey::Code(key) => kvm::hid(key),
            _ => None,
        },
    };
    let Some(hid) = hid else {
        st.os.unmapped.fetch_add(1, Ordering::Relaxed);
        st.os.last_unmapped.store(u64::from(k.vkCode & 0xFFFF) << 32 | u64::from(k.scanCode & 0xFFFF) << 16 | u64::from(k.flags & 0xFFFF), Ordering::Relaxed);
        return SWALLOW;
    };
    if (0xE0..=0xE7).contains(&hid) {
        let bit = 1 << (hid - 0xE0);
        MODS.set(if down { MODS.get() | bit } else { MODS.get() & !bit });
    }
    if down && kvm::escape(hid, MODS.get()) {
        st.end(None, Why::ESCAPE);
        return SWALLOW;
    }
    if down && hid == 0x29 {
        st.os.esc_missed.fetch_add(1, Ordering::Relaxed);
        st.os.esc_mods.store(MODS.get().into(), Ordering::Relaxed);
    }
    st.send(Captured::Key(hid, down));
    SWALLOW
}

/// Puts both hooks on a thread of their own with a message loop. Ok with that thread's id, for
/// [`stop`].
/// shortcut: a hook Windows removes for being slow (`LowLevelHooksTimeout`) isn't put back; the
/// callbacks only queue, and input then simply stays here. Re-hook if the device test shows it.
pub(super) fn start(st: &Arc<State>) -> Result<usize, String> {
    let user = Arc::into_raw(st.clone()) as *mut State;
    if ST.compare_exchange(null_mut(), user, Ordering::AcqRel, Ordering::Acquire).is_err() {
        // SAFETY: the Arc made just above.
        drop(unsafe { Arc::from_raw(user) });
        return Err("already crossing".into());
    }
    let (made_tx, made) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new().name("aun-hook".into()).spawn(move || {
        // SAFETY: hooks made, served and removed on this thread; `ST` holds the raw Arc until
        // both are off, then this thread takes it back.
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let m = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
            let k = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
            let mut msg: MSG = std::mem::zeroed();
            // The thread's message queue, there before its id goes out for `stop`.
            PeekMessageW(&mut msg, null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
            if !m.is_null() && !k.is_null() {
                let _ = made_tx.send(Some(GetCurrentThreadId() as usize));
                while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
            } else {
                crate::log(format_args!("cross: hook not made (error {})", windows_sys::Win32::Foundation::GetLastError()));
                let _ = made_tx.send(None);
            }
            for h in [m, k] {
                if !h.is_null() {
                    UnhookWindowsHookEx(h);
                }
            }
            let st = Arc::from_raw(ST.swap(null_mut(), Ordering::AcqRel));
            st.end(None, Why::STOPPED);
        }
    });
    if let Err(e) = spawned {
        // SAFETY: no thread took it.
        drop(unsafe { Arc::from_raw(ST.swap(null_mut(), Ordering::AcqRel)) });
        return Err(e.to_string());
    }
    made.recv().ok().flatten().ok_or_else(|| "input hooks not made".into())
}

/// Ends the hook thread and waits (up to 1 s) for its hooks to be off, so a `Cross` started
/// right after (the side switched) can put its own on.
pub(super) fn stop(_st: &State, thread: usize) {
    // SAFETY: a plain call; the hook thread ends its loop, unhooks and lets go of the state.
    unsafe { PostThreadMessageW(thread as u32, WM_QUIT, 0, 0) };
    for _ in 0..100 {
        if ST.load(Ordering::Acquire).is_null() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
