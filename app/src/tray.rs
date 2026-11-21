//! The menu bar item (macOS) or tray icon (Windows): open the window, switch clipboard sharing,
//! quit. Windows uses Rune's `Tray`; Rune has none on macOS yet, so there it is an `NSStatusItem`
//! made here. Both are created and changed on the main thread (inside `AppHandle::update`).

use rune::AppHandle;

use crate::Ui;

const ITEMS: [&str; 4] = ["open", "copied", "clip", "quit"];

fn label(id: &str, clip: bool) -> &'static str {
    match id {
        "open" => "Aun 열기",
        "copied" => "복사한 파일 보내기",
        "clip" if clip => "클립보드 공유 끄기",
        "clip" => "클립보드 공유 켜기",
        _ => "종료",
    }
}

/// What a picked item does. Runs on the tray's thread (Windows) or the main thread (macOS).
fn pick(h: &AppHandle<Ui>, id: &str) {
    match id {
        "open" => h.open(),
        "clip" => h.update(|s, tx| s.set_clip(!s.settings.clipboard, tx)),
        "copied" => h.update(|s, _| s.send_copied(None)),
        "quit" => h.quit(),
        _ => {}
    }
}

/// Windows rebuilds its menu from these; the item for copied files shows only while some wait.
#[cfg(windows)]
pub struct Tray(rune::Tray, std::cell::Cell<(bool, bool)>);

#[cfg(windows)]
impl Tray {
    pub fn new(h: AppHandle<Ui>, clip: bool) -> Option<Tray> {
        use rune::{TrayEvent, TrayIcon, TraySpec};
        let spec = TraySpec::new("dev.aun.desk", "Aun", TrayIcon::rgba(32, 32, icon(32))).tooltip("Aun").menu(menu(clip, false));
        let tray = rune::Tray::new(spec, move |e| match e {
            TrayEvent::Activate { .. } => h.open(),
            TrayEvent::Menu(id) => pick(&h, &id),
            _ => {}
        });
        tray.map_err(|e| aun_core::log(format_args!("tray: {e}"))).ok().map(|t| Tray(t, std::cell::Cell::new((clip, false))))
    }

    pub fn set_clip(&self, clip: bool) {
        self.set((clip, self.1.get().1));
    }

    pub fn set_copied(&self, copied: bool) {
        self.set((self.1.get().0, copied));
    }

    fn set(&self, state: (bool, bool)) {
        self.1.set(state);
        self.0.set_menu(menu(state.0, state.1));
    }
}

#[cfg(windows)]
fn menu(clip: bool, copied: bool) -> Vec<rune::TrayItem> {
    let mut v: Vec<_> = ITEMS.iter().filter(|id| copied || **id != "copied").map(|id| rune::TrayItem::action(*id, label(id, clip))).collect();
    let quit = v.len() - 1;
    v.insert(quit, rune::TrayItem::Separator);
    v
}

/// Two overlapping screens in white on a blue rounded square, `n × n` straight RGBA.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn icon(n: usize) -> Vec<u8> {
    let f = n as f32 / 32.0;
    let inside = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32| x >= x0 * f && x < x1 * f && y >= y0 * f && y < y1 * f;
    let mut px = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded corners: radius 6 at 32 px.
            let (r, c) = (6.0 * f, |v: f32| v.clamp(6.0 * f, n as f32 - 6.0 * f));
            let corner = (fx - c(fx)).hypot(fy - c(fy)) > r;
            let back = inside(fx, fy, 4.0, 7.0, 20.0, 19.0) && !inside(fx, fy, 6.0, 9.0, 18.0, 17.0);
            let front = inside(fx, fy, 12.0, 13.0, 28.0, 25.0) && !inside(fx, fy, 14.0, 15.0, 26.0, 23.0);
            px.extend_from_slice(match () {
                _ if corner => &[0, 0, 0, 0],
                _ if back || front => &[255, 255, 255, 255],
                _ => &[37, 99, 235, 255],
            });
        }
    }
    px
}

#[cfg(target_os = "macos")]
pub use mac::Tray;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
    use objc2_foundation::NSString;

    type OnPick = Box<dyn Fn(&str)>;

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "AunStatusTarget"]
        #[ivars = OnPick]
        struct Target;

        impl Target {
            #[unsafe(method(aunPick:))]
            fn pick(&self, sender: &NSMenuItem) {
                if let Some(id) = ITEMS.get(sender.tag() as usize) {
                    (self.ivars())(id);
                }
            }
        }
    );

    pub struct Tray {
        item: Retained<NSStatusItem>,
        clip: Retained<NSMenuItem>,
        copied: Retained<NSMenuItem>,
        _target: Retained<Target>,
    }

    impl Tray {
        pub fn new(h: AppHandle<Ui>, clip: bool) -> Option<Tray> {
            let mtm = MainThreadMarker::new()?;
            let this = mtm.alloc::<Target>().set_ivars(Box::new(move |id: &str| pick(&h, id)) as OnPick);
            // SAFETY: NSObject's init on a freshly allocated object.
            let target: Retained<Target> = unsafe { msg_send![super(this), init] };
            let menu = NSMenu::new(mtm);
            let (mut clip_item, mut copied_item) = (None, None);
            for (i, id) in ITEMS.iter().enumerate() {
                if *id == "quit" {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                }
                // SAFETY: `aunPick:` is implemented by `target`, which the Tray keeps alive.
                let item = unsafe {
                    let item = NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), &NSString::from_str(label(id, clip)), Some(sel!(aunPick:)), &NSString::from_str(""));
                    item.setTarget(Some(&target));
                    item
                };
                item.setTag(i as isize);
                menu.addItem(&item);
                match *id {
                    "clip" => clip_item = Some(item),
                    "copied" => {
                        item.setHidden(true);
                        copied_item = Some(item);
                    }
                    _ => {}
                }
            }
            // NSVariableStatusItemLength
            let item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0);
            if let Some(button) = item.button(mtm) {
                let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("rectangle.on.rectangle"), Some(&NSString::from_str("Aun")));
                match image {
                    Some(img) => {
                        img.setTemplate(true);
                        button.setImage(Some(&img));
                    }
                    None => button.setTitle(&NSString::from_str("Aun")),
                }
            }
            item.setMenu(Some(&menu));
            let t = Tray { item, clip: clip_item?, copied: copied_item?, _target: target };
            t.set_clip(clip);
            Some(t)
        }

        pub fn set_clip(&self, clip: bool) {
            self.clip.setTitle(&NSString::from_str(label("clip", clip)));
            self.clip.setState(if clip { NSControlStateValueOn } else { NSControlStateValueOff });
        }

        pub fn set_copied(&self, copied: bool) {
            self.copied.setHidden(!copied);
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct Tray;

#[cfg(not(any(windows, target_os = "macos")))]
impl Tray {
    pub fn new(_: AppHandle<Ui>, _: bool) -> Option<Tray> {
        None
    }
    pub fn set_clip(&self, _: bool) {}
    pub fn set_copied(&self, _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_icon_matches_its_size_and_has_clear_corners() {
        // rune::Tray refuses pixels that aren't width × height × 4 bytes.
        for n in [16, 32] {
            let px = icon(n);
            assert_eq!(px.len(), n * n * 4);
            assert_eq!(px[3], 0, "the top-left corner is transparent");
        }
    }
}
