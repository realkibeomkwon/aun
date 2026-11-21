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
