//! What the user switched, kept as `key=value` lines in `settings.txt` beside the pairing data.
//! Unknown keys are ignored, so a later version can add some without breaking this one.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub clipboard: bool,
    /// macOS: show the Dock icon. Off, Aun lives in the menu bar only.
    pub dock: bool,
    /// Where received files go; None is the Downloads folder.
    pub downloads: Option<PathBuf>,
    /// Share copies that password managers mark private. Off: they stay on this device.
    pub secrets: bool,
    /// Let paired devices move this device's pointer. Off until switched on.
    pub kvm: bool,
    /// Remote control from here: send ⌘ as Ctrl and Ctrl as ⌘ (the Windows key), so a Mac's ⌘C
    /// is Ctrl+C on a PC and a PC's Ctrl+C is ⌘C on a Mac.
    pub swap: bool,
    /// Move the pointer over to the peer past a screen edge, with this keyboard going with it.
    /// Off until switched on: it takes this device's input while across.
    pub cross: bool,
    /// Where the peer's screen is from here: 0 left, 1 right (as in `KvmEnter`).
    pub side: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { clipboard: true, dock: true, downloads: None, secrets: false, kvm: false, swap: true, cross: false, side: 1 }
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        let mut s = Settings::default();
        for line in std::fs::read_to_string(dir.join("settings.txt")).unwrap_or_default().lines() {
            match line.split_once('=') {
                Some(("clipboard", v)) => s.clipboard = v == "1",
                Some(("dock", v)) => s.dock = v == "1",
                Some(("secrets", v)) => s.secrets = v == "1",
                Some(("kvm", v)) => s.kvm = v == "1",
                Some(("swap", v)) => s.swap = v == "1",
                Some(("cross", v)) => s.cross = v == "1",
                Some(("side", "0")) => s.side = 0,
                Some(("downloads", v)) if !v.is_empty() => s.downloads = Some(v.into()),
                _ => {}
            }
        }
        s
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut text = format!("clipboard={}\ndock={}\nsecrets={}\nkvm={}\nswap={}\ncross={}\nside={}\n", self.clipboard as u8, self.dock as u8, self.secrets as u8, self.kvm as u8, self.swap as u8, self.cross as u8, self.side);
        // One line per key: a folder whose name has a line break isn't kept.
        if let Some(d) = self.downloads.as_ref().and_then(|d| d.to_str()).filter(|d| !d.contains(['\n', '\r'])) {
            text += &format!("downloads={d}\n");
        }
        aun_core::store::write_atomic(&dir.join("settings.txt"), text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("aun-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir), Settings::default(), "no file: defaults");
        let off = Settings { clipboard: false, dock: false, downloads: Some("/Users/me/받은 파일=새".into()), secrets: true, kvm: true, swap: false, cross: true, side: 0 };
        off.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), off);
        std::fs::write(dir.join("settings.txt"), "junk\nclipboard=0\nfuture=7\n").unwrap();
        assert_eq!(Settings::load(&dir), Settings { clipboard: false, dock: true, downloads: None, secrets: false, kvm: false, swap: true, cross: false, side: 1 });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
