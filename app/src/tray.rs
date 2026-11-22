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
        let spec = TraySpec::new("dev.aun.desk", "Aun", TrayIcon::rgba(32, 32, TRAY_ICON.to_vec())).tooltip("Aun").menu(menu(clip, false));
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

/// The app icon's square at 32 × 32, straight RGBA, drawn by `scripts/icon.swift`.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
const TRAY_ICON: &[u8] = include_bytes!("../../assets/tray.rgba");

#[cfg(target_os = "macos")]
pub use mac::Tray;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
    use objc2_foundation::{NSData, NSSize, NSString};

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
                // The app icon's two pointers as a template image, 36 px for 18 pt (`scripts/icon.swift`).
                let png = NSData::with_bytes(include_bytes!("../../assets/menubar.png"));
                match NSImage::initWithData(mtm.alloc(), &png) {
                    Some(img) => {
                        img.setSize(NSSize::new(18.0, 18.0));
                        img.setTemplate(true);
                        img.setAccessibilityDescription(Some(&NSString::from_str("Aun")));
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
        assert_eq!(TRAY_ICON.len(), 32 * 32 * 4);
        assert_eq!(TRAY_ICON[3], 0, "the top-left corner is transparent");
        assert!(TRAY_ICON[(16 * 32 + 16) * 4 + 3] > 250, "the middle is opaque");
    }
}
