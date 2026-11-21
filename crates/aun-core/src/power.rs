//! The OS saying it will sleep or woke, passed to the node: links close before the sleep instead
//! of looking alive to the peer until 35 s of silence, and stay closed through DarkWakes.

use crate::node::Node;

/// Runs for the life of the process. macOS: delivered on the main thread's run loop.
#[cfg(target_os = "macos")]
pub fn watch(node: Node) {
    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification};
    // NSWorkspace's own center: these never reach the default one.
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    for (name, asleep) in [(unsafe { NSWorkspaceWillSleepNotification }, true), (unsafe { NSWorkspaceDidWakeNotification }, false)] {
        let n = node.clone();
        let block = RcBlock::new(move |_| n.set_sleeping(asleep));
        // SAFETY: no object filter, no queue (posting thread); the block only touches the node.
        let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) };
        // Observed for the life of the process.
        std::mem::forget(token);
    }
}

#[cfg(windows)]
pub fn watch(node: Node) {
    use windows_sys::Win32::System::Power::{DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND};
    unsafe extern "system" fn on_power(ctx: *const core::ffi::c_void, kind: u32, _: *const core::ffi::c_void) -> u32 {
        // SAFETY: `ctx` is the leaked Node below, alive for the life of the process.
        let node = unsafe { &*(ctx as *const Node) };
        match kind {
            PBT_APMSUSPEND => node.set_sleeping(true),
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => node.set_sleeping(false),
            _ => {}
        }
        0
    }
    // Both leaked: the registration lasts as long as the process.
    let params = Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS { Callback: Some(on_power), Context: Box::into_raw(Box::new(node)).cast() }));
    let mut handle = std::ptr::null_mut();
    // SAFETY: `params` outlives the registration; the callback matches the documented signature.
    let err = unsafe { PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK, (params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast(), &mut handle) };
    if err != 0 {
        crate::log(format_args!("sleep notifications unavailable: error {err}"));
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn watch(_: Node) {}

/// Time since the last keyboard or mouse input. A DarkWake has none, so this getting shorter
/// while the node thinks it is asleep means someone is at the machine: a missed wake notice.
#[cfg(target_os = "macos")]
pub fn idle() -> Option<std::time::Duration> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    // SAFETY: plain query. Combined session state (0), any input event type (!0).
    let s = unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) };
    std::time::Duration::try_from_secs_f64(s).ok()
}

#[cfg(windows)]
pub fn idle() -> Option<std::time::Duration> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info` is sized for the call; both read the same tick clock.
    unsafe { (GetLastInputInfo(&mut info) != 0).then(|| std::time::Duration::from_millis(GetTickCount().wrapping_sub(info.dwTime) as u64)) }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn idle() -> Option<std::time::Duration> {
    None
}
