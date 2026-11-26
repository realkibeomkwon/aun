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

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{REARM, State, Why};
use crate::kvm::{self, Captured};

/// The state the hooks serve: set while they are on (one `Cross` at a time; the app drops the
/// old one first). Hooks take no user pointer.
static ST: AtomicPtr<State> = AtomicPtr::new(null_mut());

/// The window put under the parked cursor while across, for touchpad scrolling: a precision
/// touchpad's scroll mostly skips `WH_MOUSE_LL` (2026-11-26: 0, 0 and 1 scrolls taken in three
/// crossings) and comes as `WM_MOUSEWHEEL` to the window under the cursor. Made on the hook
/// thread, which dispatches its messages.
static OVERLAY: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(null_mut());
/// Its side, in pixels, centred on the parked cursor.
const OVERLAY_SIZE: i32 = 100;

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
    /// Every keyboard hook call while across, before any check, and the ones let through for
    /// each reason: whether the hook is called at all for the keys that went missing (2026-11-26:
    /// only with Aun's own window in front), and the virtual key of the first let through.
    calls: AtomicU32,
    passed: [AtomicU32; 3],
    first_passed: AtomicU32,
    /// Scrolls the overlay window took (of the crossing's scrolls): whether touchpad scrolling
    /// reaches it.
    overlay: AtomicU32,
    /// Whether this process's window was in front as the crossing started.
    pub(super) front_is_aun: std::sync::atomic::AtomicBool,
}

/// What [`Shared`] holds for this crossing, for its last log line; cleared for the next.
pub(super) fn diag(st: &State) -> String {
    let d = &st.os;
    let (n, last) = (d.unmapped.swap(0, Ordering::Relaxed), d.last_unmapped.swap(0, Ordering::Relaxed));
    let (esc, mods) = (d.esc_missed.swap(0, Ordering::Relaxed), d.esc_mods.swap(0, Ordering::Relaxed));
    let calls = d.calls.swap(0, Ordering::Relaxed);
    let [neg, mark, idle] = d.passed.each_ref().map(|a| a.swap(0, Ordering::Relaxed));
    let first = d.first_passed.swap(0, Ordering::Relaxed);
    let front = d.front_is_aun.load(Ordering::Relaxed);
    let overlay = d.overlay.swap(0, Ordering::Relaxed);
    format!(
        "; {overlay} scrolls by the overlay window; {n} keys with no wire key, last vk {:#x} scan {:#x} flags {:#x}; {esc} Esc not escaping, last with modifiers {mods:#04x}; keyboard hook called {calls} times across, let through {neg} for code, {mark} as Aun's own, {idle} as not across, first vk {first:#x}; Aun in front at the start: {front}",
        last >> 32,
        (last >> 16) & 0xFFFF,
        last & 0xFFFF
    )
}

pub(super) fn cursor() -> (f64, f64) {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: a plain read of the cursor.
    unsafe { GetCursorPos(&mut p) };
    (p.x.into(), p.y.into())
}

/// The cursor back at `(x, y)`: it was never taken from the mouse, only kept still. The overlay
/// window goes away (from any thread, so without waiting on the hook thread).
pub(super) fn give_back((x, y): (f64, f64)) {
    // SAFETY: plain calls taking values; a null window is skipped.
    unsafe {
        let w = OVERLAY.load(Ordering::Acquire);
        if !w.is_null() {
            ShowWindowAsync(w, SW_HIDE);
        }
        SetCursorPos(x as i32, y as i32);
    }
}

/// A crossing started (on the sender thread, not the hook's). With Aun's own window in front,
/// Windows doesn't call this process's keyboard hook for most keys (2026-11-26: across, `a` and
/// the first Esc never reached it, 8 calls for 13 keys pressed; with Notepad in front all came;
/// why is not known): the shell's desktop takes the front instead. Aun is the front process, so
/// it may hand the front on.
pub(super) fn entered(st: &State) {
    if !st.os.front_is_aun.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: plain calls; a null shell window (no Explorer) is left alone.
    let moved = unsafe {
        let shell = GetShellWindow();
        !shell.is_null() && SetForegroundWindow(shell) != 0
    };
    crate::log(format_args!("cross: Aun's window was in front, the desktop {}", if moved { "is now" } else { "could not be put in front" }));
}

/// Whether the window in front belongs to this process.
fn front_is_aun() -> bool {
    let mut pid = 0;
    // SAFETY: plain reads.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid == std::process::id()
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
        st.os.front_is_aun.store(front_is_aun(), Ordering::Relaxed);
        MODS.set(0);
        WHEEL.set((0, 0));
        // SAFETY: a plain call; the point read back is where it landed (a gap between screens
        // moves it).
        unsafe { SetCursorPos(((b.x0 + b.x1) / 2.0) as i32, ((b.y0 + b.y1) / 2.0) as i32) };
        let (x, y) = cursor();
        st.lock_at((x, y));
        let w = OVERLAY.load(Ordering::Acquire);
        if !w.is_null() {
            let (x, y) = (x as i32 - OVERLAY_SIZE / 2, y as i32 - OVERLAY_SIZE / 2);
            // SAFETY: a plain call on this thread's window; never activated, or Windows would skip
            // the keyboard hook as with Aun's window in front (`entered`).
            unsafe { SetWindowPos(w, HWND_TOPMOST, x, y, OVERLAY_SIZE, OVERLAY_SIZE, SWP_NOACTIVATE | SWP_SHOWWINDOW) };
        }
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
        // A wheel swallowed here never reaches the overlay window: no scroll counts twice.
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => wheel(msg, units),
        _ => return SWALLOW, // side buttons: not on the wire
    };
    if !matches!(c, Captured::Wheel(0, 0)) {
        st.send(c);
    }
    SWALLOW
}

/// A wheel message's 120-unit `units` as wire pixels, the rest kept for the next. Wire: positive
/// dy scrolls up, positive dx scrolls left (as the receivers read it).
fn wheel(msg: u32, units: i32) -> Captured {
    let (mut rx, mut ry) = WHEEL.get();
    let c = if msg == WM_MOUSEHWHEEL { Captured::Wheel(super::wheel_px(&mut rx, -units), 0) } else { Captured::Wheel(0, super::wheel_px(&mut ry, units)) };
    WHEEL.set((rx, ry));
    c
}

unsafe extern "system" fn overlay(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let st = ST.load(Ordering::Acquire);
            // SAFETY: as in `mouse`: set while this thread runs, and the `State` outlives it.
            if let Some(st) = unsafe { st.as_ref() }.filter(|st| st.crossed.load(Ordering::SeqCst)) {
                st.os.overlay.fetch_add(1, Ordering::Relaxed);
                let c = wheel(msg, i32::from((w >> 16) as u16 as i16));
                if !matches!(c, Captured::Wheel(0, 0)) {
                    st.send(c);
                }
            }
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        // SAFETY: the default for the rest.
        _ => unsafe { DefWindowProcW(h, msg, w, l) },
    }
}

/// The overlay window, hidden; null if it can't be made (touchpad scrolling then stays here).
/// Near-invisible, not transparent: a layered window passes the mouse through where alpha is 0.
unsafe fn make_overlay(module: HINSTANCE) -> HWND {
    let name: Vec<u16> = "AunScroll\0".encode_utf16().collect();
    // SAFETY: plain calls; the class name outlives them (copied by Windows).
    unsafe {
        let class = WNDCLASSW { lpfnWndProc: Some(overlay), hInstance: module, hCursor: LoadCursorW(null_mut(), IDC_ARROW), lpszClassName: name.as_ptr(), ..Default::default() };
        RegisterClassW(&class);
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED;
        let w = CreateWindowExW(ex, name.as_ptr(), name.as_ptr(), WS_POPUP, 0, 0, OVERLAY_SIZE, OVERLAY_SIZE, null_mut(), null_mut(), module, std::ptr::null());
        if w.is_null() {
            crate::log(format_args!("cross: overlay window not made (error {})", windows_sys::Win32::Foundation::GetLastError()));
        } else {
            SetLayeredWindowAttributes(w, 0, 1, LWA_ALPHA);
        }
        w
    }
}

unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let st = ST.load(Ordering::Acquire);
    // SAFETY: as in `mouse`; only atomics read here.
    if code < 0 && !st.is_null() && unsafe { &*st }.crossed.load(Ordering::SeqCst) {
        let d = unsafe { &(*st).os };
        d.calls.fetch_add(1, Ordering::Relaxed);
        d.passed[0].fetch_add(1, Ordering::Relaxed);
    }
    if code < 0 || st.is_null() {
        // SAFETY: passing the event on, as the hook contract asks.
        return unsafe { CallNextHookEx(null_mut(), code, w, l) };
    }
    // SAFETY: as in `mouse`.
    let (st, k) = unsafe { (&*st, &*(l as *const KBDLLHOOKSTRUCT)) };
    let across = st.crossed.load(Ordering::SeqCst);
    if across {
        st.os.calls.fetch_add(1, Ordering::Relaxed);
    }
    let mark = k.dwExtraInfo == kvm::MARK as usize;
    if mark || !across {
        // Diagnostic: counted from the first key of a crossing on (`calls` counts it already).
        if st.os.calls.load(Ordering::Relaxed) > 0 {
            st.os.passed[if mark { 1 } else { 2 }].fetch_add(1, Ordering::Relaxed);
            let _ = st.os.first_passed.compare_exchange(0, k.vkCode, Ordering::Relaxed, Ordering::Relaxed);
        }
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
            let ov = make_overlay(module);
            OVERLAY.store(ov, Ordering::Release);
            let m = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
            let k = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
            let mut msg: MSG = std::mem::zeroed();
            // The thread's message queue, there before its id goes out for `stop`.
            PeekMessageW(&mut msg, null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
            if !m.is_null() && !k.is_null() {
                let _ = made_tx.send(Some(GetCurrentThreadId() as usize));
                while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&msg);
                }
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
            OVERLAY.store(null_mut(), Ordering::Release);
            if !ov.is_null() {
                DestroyWindow(ov);
            }
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
